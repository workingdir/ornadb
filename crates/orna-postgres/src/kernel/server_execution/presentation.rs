use super::*;

/// Executes one closed standard `orna.server-json-encode` artifact.
///
/// This engine is reachable only from a pinned standard
/// [`FunctionRevisionRecord`], its already bound [`FunctionArgument`], the
/// active revision it executes against, and the opaque codec registry of the
/// active verified standard. It dispatches purely by checked artifact kind,
/// format, and version, then validates the artifact against the pinned
/// standard presenter signature: decode pins the function's parameter
/// identity and the resolved `std.json.Value` value type, and the signature
/// validator requires the fixed ADR 0057 `std.json.encode` shape. It never
/// matches a function by Rust name or [`FunctionId`], executes SQL, or opens
/// a PostgreSQL row.
///
/// The bound value converts to JSON without loss (integers, bigints, floats,
/// booleans, text, bytes as base64, references as an explicit
/// `$ref`/`$type` object, lists, maps, and null), and the result is one
/// `std.io.ByteStream` opaque value whose payload follows the ADR 0058 codec
/// framing (`ORNA-BYTE-STREAM/1 <media-type-len:u32 be> <media-type>
/// <len:u32 be> <bytes>`) with media type `application/json`.
///
/// ADR 0057 step 7 wires the presenter engines into the sealed output
/// resolution; the sealed route (`dispatch_sealed_sys_invoke`) is the sole
/// caller of this engine.
pub(crate) fn execute_standard_json_encode(
    function: &FunctionDefinition,
    revision: &FunctionRevisionRecord,
    arguments: &[FunctionArgument],
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<RuntimeValue, PostgresKernelError> {
    execute_standard_json_encode_bound(
        function,
        revision,
        StandardJsonEncodeBinding::Arguments(arguments),
        active,
        registry,
    )
}

/// One validated binding form for the closed JSON presenter.
///
/// Ordinary execution receives a [`FunctionArgument`], whose general-purpose
/// constructor intentionally rejects typed nulls. The sealed presenter route
/// has already obtained the canonical result and therefore binds that result
/// directly, after this engine checks the same pinned parameter identity.
enum StandardJsonEncodeBinding<'a> {
    Arguments(&'a [FunctionArgument]),
    Value {
        parameter: ParameterId,
        value: &'a RuntimeValue,
    },
}

/// Executes the closed JSON presenter after selecting one of its validated
/// binding seams. All artifact, revision, function-signature, and parameter
/// checks stay in this common path for both ordinary and sealed execution.
fn execute_standard_json_encode_bound(
    function: &FunctionDefinition,
    revision: &FunctionRevisionRecord,
    binding: StandardJsonEncodeBinding<'_>,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<RuntimeValue, PostgresKernelError> {
    let artifact = revision.artifact();
    if artifact.kind() != ExecutableArtifactKind::Server {
        return Err(artifact_error(
            function.id(),
            "current revision must contain a SERVER artifact",
        ));
    }
    if artifact.format() != server_json_encode::FORMAT_IDENTITY {
        return Err(artifact_error(
            function.id(),
            "current SERVER artifact must use orna.server-json-encode",
        ));
    }
    if artifact.version() != server_json_encode::FORMAT_VERSION {
        return Err(artifact_error(
            function.id(),
            "current SERVER artifact must use orna.server-json-encode version 1",
        ));
    }
    if revision.language_version() != server_json_encode::LANGUAGE_VERSION_IDENTITY {
        return Err(artifact_error(
            function.id(),
            "current SERVER revision must use the json-encode language version",
        ));
    }
    let parameter = validate_standard_json_encode_signature(function)?;
    JsonEncodePlan::decode(artifact.payload(), parameter, STD_JSON_VALUE_TYPE_ID)
        .map_err(ServerSelectError::JsonEncodeDecode)
        .map_err(server_error)?;
    let value = match binding {
        StandardJsonEncodeBinding::Arguments(arguments) => {
            validate_standard_json_encode_argument(parameter, arguments)?
        }
        StandardJsonEncodeBinding::Value {
            parameter: bound_parameter,
            value,
        } => {
            if bound_parameter != parameter {
                return Err(argument_error(
                    Some(bound_parameter),
                    "standard json-encode arguments must bind the pinned parameter identity",
                ));
            }
            value
        }
    };
    let json = encode_json_value(active, value)
        .map_err(|rule| ServerSelectError::Presenter { rule })
        .map_err(server_error)?;
    let json_bytes = serde_json::to_vec(&json).map_err(|_| {
        server_error(ServerSelectError::Presenter {
            rule: "std.json.encode produced an unrepresentable JSON document",
        })
    })?;
    let payload = frame_byte_stream(b"application/json", &json_bytes);
    let opaque = OpaqueValue::new(active, registry, STD_IO_BYTE_STREAM_TYPE_ID, &payload)
        .map_err(ServerSelectError::PresenterOpaque)
        .map_err(server_error)?;
    Ok(RuntimeValue::Opaque(opaque))
}

/// Executes one closed standard `orna.server-terminal-table` artifact.
///
/// This engine is reachable only from a pinned standard
/// [`FunctionRevisionRecord`], the bound `std.data.Rows` input (the validated
/// [`ResultRows`] result set itself, which cannot ride the value channel),
/// the active revision it executes against, and the opaque codec registry of
/// the active verified standard. It dispatches purely by checked artifact
/// kind, format, and version, then validates the artifact against the pinned
/// standard presenter signature: decode pins the function's parameter
/// identity and the resolved `std.data.Rows` value type, and the signature
/// validator requires the fixed ADR 0057 `std.terminal.present_table` shape.
/// It never matches a function by Rust name or [`FunctionId`], executes SQL,
/// or opens a PostgreSQL row.
///
/// The bound rows render as the fixed plain-text table (column headers,
/// aligned values, and a trailing row count), and the result is one
/// `std.terminal.Document` opaque value whose payload follows the ADR 0058
/// codec framing (`ORNA-TERMINAL-DOCUMENT/1 <len:u32 be> <utf-8>`).
///
/// ADR 0057 step 7 wires the presenter engines into the sealed output
/// resolution; the sealed route (`dispatch_sealed_sys_invoke`) is the sole
/// caller of this engine.
pub(super) fn execute_standard_terminal_table(
    function: &FunctionDefinition,
    revision: &FunctionRevisionRecord,
    rows: &ResultRows,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<RuntimeValue, PostgresKernelError> {
    let artifact = revision.artifact();
    if artifact.kind() != ExecutableArtifactKind::Server {
        return Err(artifact_error(
            function.id(),
            "current revision must contain a SERVER artifact",
        ));
    }
    if artifact.format() != server_terminal_table::FORMAT_IDENTITY {
        return Err(artifact_error(
            function.id(),
            "current SERVER artifact must use orna.server-terminal-table",
        ));
    }
    if artifact.version() != server_terminal_table::FORMAT_VERSION {
        return Err(artifact_error(
            function.id(),
            "current SERVER artifact must use orna.server-terminal-table version 1",
        ));
    }
    if revision.language_version() != server_terminal_table::LANGUAGE_VERSION_IDENTITY {
        return Err(artifact_error(
            function.id(),
            "current SERVER revision must use the terminal-table language version",
        ));
    }
    let parameter = validate_standard_terminal_present_table_signature(function)?;
    TerminalTablePlan::decode(artifact.payload(), parameter, STD_DATA_ROWS_TYPE_ID)
        .map_err(ServerSelectError::TerminalTableDecode)
        .map_err(server_error)?;
    let document = render_terminal_table(active, rows)
        .map_err(|rule| ServerSelectError::Presenter { rule })
        .map_err(server_error)?;
    let payload = frame_terminal_document(&document);
    let opaque = OpaqueValue::new(active, registry, STD_TERMINAL_DOCUMENT_TYPE_ID, &payload)
        .map_err(ServerSelectError::PresenterOpaque)
        .map_err(server_error)?;
    Ok(RuntimeValue::Opaque(opaque))
}

/// Executes one closed standard `orna.server-csv-encode` artifact.
///
/// This engine is reachable only from a pinned standard
/// [`FunctionRevisionRecord`], the bound `std.data.Rows` input (the validated
/// [`ResultRows`] result set itself, which cannot ride the value channel),
/// the active revision it executes against, and the opaque codec registry of
/// the active verified standard. It dispatches purely by checked artifact
/// kind, format, and version, then validates the artifact against the pinned
/// standard presenter signature: decode pins the function's parameter
/// identity and the resolved `std.data.Rows` value type, and the signature
/// validator requires the fixed ADR 0067 `std.csv.encode` shape.
///
/// The bound rows render as one CSV document (header row of column names,
/// one row per result row, RFC-4180-style quoting), and the result is one
/// `std.io.ByteStream` opaque value whose payload follows the ADR 0058 codec
/// framing (`ORNA-BYTE-STREAM/1 <media-type:u32 be> <media-type>
/// <len:u32 be> <bytes>`) with media type `text/csv`.
pub(super) fn execute_standard_csv_encode(
    function: &FunctionDefinition,
    revision: &FunctionRevisionRecord,
    rows: &ResultRows,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<RuntimeValue, PostgresKernelError> {
    let artifact = revision.artifact();
    if artifact.kind() != ExecutableArtifactKind::Server {
        return Err(artifact_error(
            function.id(),
            "current revision must contain a SERVER artifact",
        ));
    }
    if artifact.format() != server_csv_encode::FORMAT_IDENTITY {
        return Err(artifact_error(
            function.id(),
            "current SERVER artifact must use orna.server-csv-encode",
        ));
    }
    if artifact.version() != server_csv_encode::FORMAT_VERSION {
        return Err(artifact_error(
            function.id(),
            "current SERVER artifact must use orna.server-csv-encode version 1",
        ));
    }
    if revision.language_version() != server_csv_encode::LANGUAGE_VERSION_IDENTITY {
        return Err(artifact_error(
            function.id(),
            "current SERVER revision must use the csv-encode language version",
        ));
    }
    let parameter = validate_standard_csv_encode_signature(function)?;
    CsvEncodePlan::decode(artifact.payload(), parameter, STD_DATA_ROWS_TYPE_ID)
        .map_err(ServerSelectError::CsvEncodeDecode)
        .map_err(server_error)?;
    let document = render_csv_document(active, rows)
        .map_err(|rule| ServerSelectError::Presenter { rule })
        .map_err(server_error)?;
    let payload = frame_byte_stream(b"text/csv", document.as_bytes());
    let opaque = OpaqueValue::new(active, registry, STD_IO_BYTE_STREAM_TYPE_ID, &payload)
        .map_err(ServerSelectError::PresenterOpaque)
        .map_err(server_error)?;
    Ok(RuntimeValue::Opaque(opaque))
}

/// One closed presentation failure from the sealed output route (ADR 0057
/// step 7).
///
/// Both the unresolved-requirement and the no-path failures are presentation
/// errors (spec exit 5); the sealed dispatch discloses neither variant in its
/// public result. The `Kernel` variant carries only closed engine or
/// invariant failures.
#[derive(Debug)]
pub(crate) enum SealedPresentationError {
    /// The output requirement did not resolve against the presenter registry:
    /// `ORNA0702` (spec exit 5).
    OutputResolution(OutputResolutionError),
    /// The resolved presenter's input pattern does not accept the canonical
    /// result: `ORNA0701` (spec exit 5).
    NoPath,
    /// A closed presenter-engine or registry-invariant failure.
    Kernel(PostgresKernelError),
}

impl SealedPresentationError {
    /// Returns the stable spec code for this presentation failure.
    #[cfg(test)]
    pub(crate) const fn spec_code(&self) -> &'static str {
        match self {
            Self::OutputResolution(error) => error.spec_code(),
            Self::NoPath => "ORNA0701",
            Self::Kernel(_) => "ORNA0702",
        }
    }
    /// Returns the spec exit code for a presentation error.
    #[cfg(test)]
    pub(crate) const fn exit_code(&self) -> u8 {
        5
    }
}

impl fmt::Display for SealedPresentationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutputResolution(error) => write!(formatter, "{error}"),
            Self::NoPath => formatter.write_str("no presenter accepts the canonical result type"),
            Self::Kernel(error) => write!(formatter, "{error}"),
        }
    }
}

/// The immutable sealed presenter registry (ADR 0057 step 7, ADR 0067).
///
/// The V1-V7 compatibility snapshots do not provide retained table-presenter
/// records, so the route constructs the compatibility selector records here: alias
/// `json` -> `std.json.encode` (input `std.json.Value`, output
/// `std.io.ByteStream` with media type `application/json`), alias `table` ->
/// `std.terminal.present_table` (input `std.data.Rows`, output
/// `std.terminal.Document` with media type `text/plain`), and alias `csv` ->
/// `std.csv.encode` (input `std.data.Rows`, output `std.io.ByteStream` with
/// media type `text/csv`). The table selector is only compatibility metadata:
/// V8/V9 execution resolves the function and retained executable from the
/// active verified standard in `retained_terminal_table_target`. All entries
/// stream nothing and carry the default priority.
fn sealed_presenter_registry() -> &'static PresenterRegistry {
    static REGISTRY: OnceLock<PresenterRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let json = PresenterEntry::new(
            String::from("json"),
            STD_JSON_ENCODE_FUNCTION_ID,
            STD_JSON_VALUE_TYPE_ID,
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
            Some(String::from("application/json")),
            false,
            0,
        )
        .expect("the fixed json presenter entry is valid");
        let table = PresenterEntry::new(
            String::from("table"),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            STD_DATA_ROWS_TYPE_ID,
            orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
            Some(String::from("text/plain")),
            false,
            0,
        )
        .expect("the fixed table presenter entry is valid");
        let csv = PresenterEntry::new(
            String::from("csv"),
            STD_CSV_ENCODE_FUNCTION_ID,
            STD_DATA_ROWS_TYPE_ID,
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
            Some(String::from("text/csv")),
            false,
            0,
        )
        .expect("the fixed csv presenter entry is valid");
        PresenterRegistry::new(vec![json, table, csv])
            .expect("the fixed presenter registry is valid")
    })
}

/// Resolves a qualified presenter input type against the active application
/// catalogue first, then the exact verified standard snapshot pinned by the
/// active catalogue-hash context.
///
/// A name present in both catalogues is deliberately not silently selected.
/// Only multiple aliases tied at the highest presenter priority are reported
/// as `Ambiguous`; a collision with zero or one matching presenter aliases is
/// retained as the closed unresolved type-name error instead of fabricating a
/// presenter tie.
pub(super) fn resolve_sealed_presenter_type_name(
    name: &QualifiedSemanticName,
    active: &ActiveDatabaseRevision,
) -> Result<TypeId, OutputResolutionError> {
    let lookup = TypeLookupName::qualified(name.clone());
    let application = active.catalogue().type_id_by_name(&lookup);
    let standard = active
        .catalogue_hash_context()
        .standard()
        .and_then(|snapshot| snapshot.catalogue().type_id_by_name(&lookup));

    match (application, standard) {
        (Some(application), Some(standard)) => {
            let mut matching = sealed_presenter_registry()
                .entries()
                .iter()
                .filter(|entry| entry.input_type() == application || entry.input_type() == standard)
                .peekable();
            let Some(first) = matching.next() else {
                return Err(OutputResolutionError::UnresolvedTypeName {
                    name: name.to_string(),
                });
            };
            let best_priority = first.priority();
            let mut aliases = vec![first.alias().to_owned()];
            for entry in matching {
                if entry.priority() < best_priority {
                    break;
                }
                aliases.push(entry.alias().to_owned());
            }
            if aliases.len() > 1 {
                Err(OutputResolutionError::Ambiguous {
                    selector: AmbiguousOutputSelector::TypeName(name.to_string()),
                    aliases,
                })
            } else {
                Err(OutputResolutionError::UnresolvedTypeName {
                    name: name.to_string(),
                })
            }
        }
        (Some(type_id), None) | (None, Some(type_id)) => Ok(type_id),
        (None, None) => Err(OutputResolutionError::UnresolvedTypeName {
            name: name.to_string(),
        }),
    }
}

/// Returns whether the client offer admits the selected sealed presenter
/// output.
///
/// Admission requires the exact output descriptor and a compatible media
/// type. The installed byte-stream sink advertises `application/octet-stream`
/// and consumes raw bytes of any byte stream, so it is the one generic media
/// fallback; all other media types must match exactly.
fn sealed_output_sink_matches(
    entry: &PresenterEntry,
    client_offer: &InvocationClientOffer,
) -> bool {
    let output_descriptor = TypeDescriptor::named(entry.output_type());
    client_offer.sink_offers().iter().any(|offer| {
        if offer.descriptor() != &output_descriptor {
            return false;
        }
        if entry.streaming() && !offer.streaming() {
            return false;
        }
        match entry.media_type() {
            Some(media_type) => offer.media_types().iter().any(|offered| {
                offered == media_type
                    || (entry.output_type() == STD_IO_BYTE_STREAM_TYPE_ID
                        && offered == "application/octet-stream")
            }),
            None => false,
        }
    })
}

/// Resolves one sealed output requirement and presents the canonical result
/// through the matched presenter engine (ADR 0057 step 7).
///
/// The requirement resolves against the sealed presenter registry with the
/// alias > media-type > type-name precedence, then the matched presenter's
/// input pattern is checked against the canonical result: `std.json.encode`
/// accepts every argument the closed value channel can carry (any
/// json-convertible flat value), while `std.terminal.present_table` and
/// `std.csv.encode` accept the canonical result only when it converts to a
/// bounded `ResultRows`; an opaque value with the exact `std.data.Rows` type
/// is decoded back to its complete shape, while other values use the legacy
/// one-column, one-row `result` wrapper. An unresolved alias,
/// media type, or type name is [`SealedPresentationError::OutputResolution`]
/// (`ORNA0702`); a result the matched presenter cannot accept is
/// [`SealedPresentationError::NoPath`] (`ORNA0701`). The presented opaque
/// value replaces the canonical value in the final `ValueBatch`.
pub(crate) fn present_sealed_standard_output(
    requirement: &InvocationOutputRequirement,
    value: RuntimeValue,
    client_offer: &InvocationClientOffer,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<RuntimeValue, SealedPresentationError> {
    let presenter_registry = sealed_presenter_registry();
    let entry = if requirement.alias().is_none() && requirement.media_type().is_none() {
        match requirement.type_selector() {
            Some(InvocationOutputTypeSelector::QualifiedName(name)) => {
                let type_id = resolve_sealed_presenter_type_name(name, active)
                    .map_err(SealedPresentationError::OutputResolution)?;
                presenter_registry
                    .resolve_input_type(type_id)
                    .map_err(|error| match error {
                        OutputResolutionError::UnresolvedTypeName { .. } => {
                            OutputResolutionError::UnresolvedTypeName {
                                name: name.to_string(),
                            }
                        }
                        other => other,
                    })
                    .map_err(SealedPresentationError::OutputResolution)?
            }
            _ => presenter_registry
                .resolve_requirement(requirement, |_| None)
                .map_err(SealedPresentationError::OutputResolution)?,
        }
    } else {
        presenter_registry
            .resolve_requirement(requirement, |name| {
                resolve_sealed_presenter_type_name(name, active).ok()
            })
            .map_err(SealedPresentationError::OutputResolution)?
    };
    let requirement_matches = match requirement.streaming() {
        InvocationStreamingRequirement::Unspecified | InvocationStreamingRequirement::Preferred => {
            true
        }
        InvocationStreamingRequirement::Required => entry.streaming(),
        InvocationStreamingRequirement::Forbidden => !entry.streaming(),
        _ => false,
    };
    if !requirement_matches {
        return Err(SealedPresentationError::NoPath);
    }
    if !sealed_output_sink_matches(entry, client_offer) {
        return Err(SealedPresentationError::NoPath);
    }
    match entry.function() {
        STD_JSON_ENCODE_FUNCTION_ID => execute_standard_json_encode_bound(
            &sealed_json_encode_definition(),
            &sealed_json_encode_revision(),
            StandardJsonEncodeBinding::Value {
                parameter: STD_JSON_ENCODE_PARAMETER_ID,
                value: &value,
            },
            active,
            registry,
        )
        .map_err(sealed_presenter_engine_error),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID => {
            let rows = sealed_result_rows(value, active, registry)?;
            if let Some((function, revision)) =
                retained_terminal_table_target(active).map_err(sealed_presenter_engine_error)?
            {
                execute_standard_terminal_table(function, revision, &rows, active, registry)
                    .map_err(sealed_presenter_engine_error)
            } else {
                execute_standard_terminal_table(
                    &sealed_terminal_table_definition(),
                    &sealed_terminal_table_revision(),
                    &rows,
                    active,
                    registry,
                )
                .map_err(sealed_presenter_engine_error)
            }
        }
        STD_CSV_ENCODE_FUNCTION_ID => {
            let rows = sealed_result_rows(value, active, registry)?;
            execute_standard_csv_encode(
                &sealed_csv_encode_definition(),
                &sealed_csv_encode_revision(),
                &rows,
                active,
                registry,
            )
            .map_err(sealed_presenter_engine_error)
        }
        other => Err(SealedPresentationError::Kernel(
            PostgresKernelError::DurableInvariant {
                relation: "sealed presenter registry",
                record: other.canonical(),
                rule: "the sealed presenter registry must name only the ADR 0057/0067 presenters",
            },
        )),
    }
}

/// Resolves the retained V8/V9 table presenter from the active verified standard.
///
/// V1-V7 intentionally return `None` so their historical synthetic presenter
/// remains explicit. Once Rows is part of the standard snapshot, a missing or
/// crossed executable is a closed target failure: the compatibility presenter
/// must not make the table sink available.
pub(super) fn retained_terminal_table_target(
    active: &ActiveDatabaseRevision,
) -> Result<Option<(&FunctionDefinition, &FunctionRevisionRecord)>, PostgresKernelError> {
    let Some(standard) = active.catalogue_hash_context().standard() else {
        return Ok(None);
    };
    if !matches!(
        standard.revision(),
        STANDARD_LIBRARY_V8_REVISION_ID | STANDARD_LIBRARY_V9_REVISION_ID
    ) {
        return Ok(None);
    }

    let function = standard
        .catalogue()
        .function_by_id(STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID)
        .ok_or_else(|| {
            server_error(ServerSelectError::FunctionNotActive {
                pair: active.pair(),
                function: STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            })
        })?;
    let executable = standard
        .executables()
        .iter()
        .find(|executable| executable.function() == STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID)
        .ok_or_else(|| {
            server_error(ServerSelectError::FunctionNotActive {
                pair: active.pair(),
                function: STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            })
        })?;
    if executable.revision().function() != function.id()
        || executable.revision().id() != function.current_revision()
    {
        return Err(server_error(ServerSelectError::FunctionNotActive {
            pair: active.pair(),
            function: STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        }));
    }
    Ok(Some((function, executable.revision())))
}

/// Converts the canonical sealed result to the bounded `ResultRows` model the
/// terminal-table and CSV engines accept.
///
/// A registered `std.data.Rows` opaque value carries the complete result shape
/// and is decoded against the already pinned active revision and registry.
/// Other values retain the legacy one-column, one-row `result` wrapper. Opaque,
/// constructed, and invocation-carrier values have no scalar path to the
/// presentation sinks (`ORNA0701`).
pub(super) fn sealed_result_rows(
    value: RuntimeValue,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<ResultRows, SealedPresentationError> {
    if let RuntimeValue::Opaque(opaque) = &value
        && opaque.opaque_type() == STD_DATA_ROWS_TYPE_ID
    {
        return decode_rows(active, registry, opaque.canonical_payload())
            .map_err(|_| SealedPresentationError::NoPath);
    }
    let RuntimeType::Flat(resolved_type) = value.runtime_type() else {
        return Err(SealedPresentationError::NoPath);
    };
    let column = ResultColumn::new("result", resolved_type, value.is_null())
        .map_err(|_| SealedPresentationError::NoPath)?;
    ResultRows::new(vec![column], vec![ResultRow::new([value])])
        .map_err(|_| SealedPresentationError::NoPath)
}

/// Classifies one closed presenter-engine failure for the sealed route.
///
/// A conversion failure inside a presenter engine means the canonical result
/// has no path to the matched sink (`ORNA0701`); every other engine failure
/// is a closed kernel or registry invariant.
fn sealed_presenter_engine_error(error: PostgresKernelError) -> SealedPresentationError {
    match error {
        PostgresKernelError::ServerSelect(ServerSelectError::Presenter { .. }) => {
            SealedPresentationError::NoPath
        }
        other => SealedPresentationError::Kernel(other),
    }
}

/// Builds the closed ADR 0057 `std.json.encode` definition the sealed route
/// executes.
///
/// The exact shape matches the engine's signature validator: SERVER domain,
/// one required non-null `std.json.Value` parameter, one single
/// `std.io.ByteStream` result, INVOKER security, READ ONLY transaction, and
/// STABLE volatility.
fn sealed_json_encode_definition() -> FunctionDefinition {
    FunctionDefinition::new(
        STD_JSON_ENCODE_FUNCTION_ID,
        QualifiedSemanticName::new(["std", "json", "encode"])
            .expect("the fixed json-encode name is qualified"),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            STD_JSON_ENCODE_PARAMETER_ID,
            "p_value",
            0,
            ResolvedType::named(STD_JSON_VALUE_TYPE_ID),
            None,
        )],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
        )),
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    )
}

/// Builds the closed ADR 0057 `std.json.encode` revision the sealed route
/// executes: the canonical `orna.server-json-encode` version 1 artifact.
fn sealed_json_encode_revision() -> FunctionRevisionRecord {
    sealed_presenter_revision(
        STD_JSON_ENCODE_FUNCTION_ID,
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        server_json_encode::LANGUAGE_VERSION_IDENTITY,
        sealed_presenter_artifact(
            server_json_encode::FORMAT_IDENTITY,
            server_json_encode::FORMAT_VERSION,
            JsonEncodePlan::new(STD_JSON_ENCODE_PARAMETER_ID, STD_JSON_VALUE_TYPE_ID)
                .expect("the fixed json-encode plan is valid")
                .encode()
                .expect("the fixed json-encode plan encodes"),
        ),
    )
}

/// Builds the closed ADR 0057 `std.terminal.present_table` definition the
/// sealed route executes.
///
/// The exact shape matches the engine's signature validator: SERVER domain,
/// one required non-null `std.data.Rows` parameter, one single
/// `std.terminal.Document` result, INVOKER security, READ ONLY transaction,
/// and STABLE volatility.
fn sealed_terminal_table_definition() -> FunctionDefinition {
    FunctionDefinition::new(
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        QualifiedSemanticName::new(["std", "terminal", "present_table"])
            .expect("the fixed present-table name is qualified"),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
            "p_rows",
            0,
            ResolvedType::named(STD_DATA_ROWS_TYPE_ID),
            None,
        )],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
        )),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    )
}

/// Builds the closed ADR 0057 `std.terminal.present_table` revision the
/// sealed route executes: the canonical `orna.server-terminal-table` version
/// 1 artifact.
fn sealed_terminal_table_revision() -> FunctionRevisionRecord {
    sealed_presenter_revision(
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        server_terminal_table::LANGUAGE_VERSION_IDENTITY,
        sealed_presenter_artifact(
            server_terminal_table::FORMAT_IDENTITY,
            server_terminal_table::FORMAT_VERSION,
            TerminalTablePlan::new(
                STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
                STD_DATA_ROWS_TYPE_ID,
            )
            .expect("the fixed terminal-table plan is valid")
            .encode()
            .expect("the fixed terminal-table plan encodes"),
        ),
    )
}

/// Builds the closed ADR 0067 `std.csv.encode` definition the sealed route
/// executes.
///
/// The exact shape matches the engine's signature validator: SERVER domain,
/// one required non-null `std.data.Rows` parameter, one single
/// `std.io.ByteStream` result, INVOKER security, READ ONLY transaction, and
/// STABLE volatility.
fn sealed_csv_encode_definition() -> FunctionDefinition {
    FunctionDefinition::new(
        STD_CSV_ENCODE_FUNCTION_ID,
        QualifiedSemanticName::new(["std", "csv", "encode"])
            .expect("the fixed csv-encode name is qualified"),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            STD_CSV_ENCODE_PARAMETER_ID,
            "p_rows",
            0,
            ResolvedType::named(STD_DATA_ROWS_TYPE_ID),
            None,
        )],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
        )),
        STD_CSV_ENCODE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    )
}

/// Builds the closed ADR 0067 `std.csv.encode` revision the sealed route
/// executes: the canonical `orna.server-csv-encode` version 1 artifact.
fn sealed_csv_encode_revision() -> FunctionRevisionRecord {
    sealed_presenter_revision(
        STD_CSV_ENCODE_FUNCTION_ID,
        STD_CSV_ENCODE_FUNCTION_REVISION_ID,
        server_csv_encode::LANGUAGE_VERSION_IDENTITY,
        sealed_presenter_artifact(
            server_csv_encode::FORMAT_IDENTITY,
            server_csv_encode::FORMAT_VERSION,
            CsvEncodePlan::new(STD_CSV_ENCODE_PARAMETER_ID, STD_DATA_ROWS_TYPE_ID)
                .expect("the fixed csv-encode plan is valid")
                .encode()
                .expect("the fixed csv-encode plan encodes"),
        ),
    )
}

/// Frames one closed presenter artifact payload as a canonical executable
/// artifact.
fn sealed_presenter_artifact(format: &str, version: u32, payload: Vec<u8>) -> ExecutableArtifact {
    let content_hash =
        artifact_payload_digest(&payload).expect("the fixed presenter artifact digests");
    ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        format,
        version,
        payload,
        content_hash,
    )
    .expect("the fixed presenter artifact is valid")
}

/// Builds one closed presenter revision record carrying the exact language
/// version and canonical artifact of the pinned ADR 0057 presenter.
fn sealed_presenter_revision(
    function: FunctionId,
    revision: FunctionRevisionId,
    language_version: &str,
    artifact: ExecutableArtifact,
) -> FunctionRevisionRecord {
    FunctionRevisionRecord::new(
        function,
        revision,
        1,
        SourceOrigin::new(SourceUnitId::from_bytes([0x91; 16]), 0, 1)
            .expect("the fixed presenter source origin is valid"),
        Sha256Digest::from_bytes([0x42; 32]),
        Sha256Digest::from_bytes([0x43; 32]),
        language_version,
        artifact,
    )
    .expect("the fixed presenter revision is valid")
}

/// Validates one pinned function against the fixed ADR 0057
/// `std.json.encode` presenter signature.
///
/// The accepted shape is exactly: SERVER domain, one required non-null
/// `std.json.Value` parameter with no default expression, one single
/// `std.io.ByteStream` result, `SECURITY INVOKER`, `TRANSACTION READ ONLY`,
/// and `VOLATILITY STABLE`. Both the parameter and the result must resolve to
/// the fixed value types. Returns the pinned parameter identity the artifact
/// must carry.
fn validate_standard_json_encode_signature(
    function: &FunctionDefinition,
) -> Result<ParameterId, PostgresKernelError> {
    validate_standard_presenter_signature(
        function,
        STD_JSON_VALUE_TYPE_ID,
        STD_IO_BYTE_STREAM_TYPE_ID,
        "standard json-encode presenters must declare exactly one required non-null std.json.Value parameter",
        "standard json-encode presenters must return a single std.io.ByteStream value",
        "standard json-encode presenters must declare one std.json.Value parameter and one std.io.ByteStream result",
    )
}

/// Validates one pinned function against the fixed ADR 0057
/// `std.terminal.present_table` presenter signature.
///
/// The accepted shape is exactly: SERVER domain, one required non-null
/// `std.data.Rows` parameter with no default expression, one single
/// `std.terminal.Document` result, `SECURITY INVOKER`, `TRANSACTION READ
/// ONLY`, and `VOLATILITY STABLE`. Both the parameter and the result must
/// resolve to the fixed value types. Returns the pinned parameter identity
/// the artifact must carry.
fn validate_standard_terminal_present_table_signature(
    function: &FunctionDefinition,
) -> Result<ParameterId, PostgresKernelError> {
    validate_standard_presenter_signature(
        function,
        STD_DATA_ROWS_TYPE_ID,
        STD_TERMINAL_DOCUMENT_TYPE_ID,
        "standard terminal-table presenters must declare exactly one required non-null std.data.Rows parameter",
        "standard terminal-table presenters must return a single std.terminal.Document value",
        "standard terminal-table presenters must declare one std.data.Rows parameter and one std.terminal.Document result",
    )
}

/// Validates one pinned function against the fixed ADR 0067
/// `std.csv.encode` presenter signature.
///
/// The accepted shape is exactly: SERVER domain, one required non-null
/// `std.data.Rows` parameter with no default expression, one single
/// `std.io.ByteStream` result, `SECURITY INVOKER`, `TRANSACTION READ
/// ONLY`, and `VOLATILITY STABLE`. Both the parameter and the result must
/// resolve to the fixed value types. Returns the pinned parameter identity
/// the artifact must carry.
fn validate_standard_csv_encode_signature(
    function: &FunctionDefinition,
) -> Result<ParameterId, PostgresKernelError> {
    validate_standard_presenter_signature(
        function,
        STD_DATA_ROWS_TYPE_ID,
        STD_IO_BYTE_STREAM_TYPE_ID,
        "standard csv-encode presenters must declare exactly one required non-null std.data.Rows parameter",
        "standard csv-encode presenters must return a single std.io.ByteStream value",
        "standard csv-encode presenters must declare one std.data.Rows parameter and one std.io.ByteStream result",
    )
}

/// Validates one pinned function against the fixed ADR 0057 presenter shape.
///
/// The accepted shape is exactly: SERVER domain, one required non-null
/// parameter with no default expression, one single result, `SECURITY
/// INVOKER`, `TRANSACTION READ ONLY`, and `VOLATILITY STABLE`. The parameter
/// must resolve to `parameter_type` and the result to `result_type`; both
/// the retained named spelling and the durable value-type identity are
/// admitted (the retained standard catalogue spells the parameter and result
/// as resolved named types, while the pinned artifacts carry the durable
/// value-type identities). Returns the pinned parameter identity the artifact
/// must carry.
fn validate_standard_presenter_signature(
    function: &FunctionDefinition,
    parameter_type: TypeId,
    result_type: TypeId,
    parameter_rule: &'static str,
    result_rule: &'static str,
    types_rule: &'static str,
) -> Result<ParameterId, PostgresKernelError> {
    if function.domain() != FunctionDomain::Server {
        return Err(server_error(ServerSelectError::FunctionDomain {
            function: function.id(),
        }));
    }
    let [parameter] = function.parameters() else {
        return Err(function_signature_error(function.id(), parameter_rule));
    };
    if parameter.default_expression().is_some() {
        return Err(function_signature_error(function.id(), parameter_rule));
    }
    let FunctionReturn::Single(result) = function.return_type() else {
        return Err(function_signature_error(function.id(), result_rule));
    };
    if !is_standard_presenter_type(&parameter.resolved_type(), parameter_type)
        || !is_standard_presenter_type(result, result_type)
    {
        return Err(function_signature_error(function.id(), types_rule));
    }
    if function.security() != FunctionSecurity::Invoker {
        return Err(function_signature_error(
            function.id(),
            "standard presenter functions must use INVOKER security",
        ));
    }
    if function.transaction() != Some(FunctionTransaction::ReadOnly) {
        return Err(function_signature_error(
            function.id(),
            "standard presenter functions must use READ ONLY transactions",
        ));
    }
    if function.volatility() != FunctionVolatility::Stable {
        return Err(function_signature_error(
            function.id(),
            "standard presenter functions must use STABLE volatility",
        ));
    }
    Ok(parameter.id())
}

/// Returns whether one resolved type is the fixed ADR 0057 presenter type.
///
/// The retained standard catalogue spells presenter parameters and results as
/// resolved named types, while the pinned presenter artifacts carry the
/// durable `Value(type_id)` identities; both denote the same fixed value
/// type, so the closed signature validator admits exactly these two forms and
/// nothing else.
fn is_standard_presenter_type(resolved_type: &ResolvedType, type_id: TypeId) -> bool {
    *resolved_type == ResolvedType::value(type_id) || *resolved_type == ResolvedType::named(type_id)
}

/// Validates the exact bound argument of one standard json-encode call.
///
/// The ordinary engine accepts exactly one argument bound to the pinned
/// parameter. Its [`FunctionArgument`] boundary rejects typed nulls; the
/// sealed route uses [`StandardJsonEncodeBinding::Value`] instead, after the
/// common artifact, signature, and parameter checks. The returned value is the
/// already bound typed value, whose conversion to JSON is the presenter's
/// closed lossless rule.
fn validate_standard_json_encode_argument(
    parameter: ParameterId,
    arguments: &[FunctionArgument],
) -> Result<&RuntimeValue, PostgresKernelError> {
    let [argument] = arguments else {
        return Err(argument_error(
            None,
            "standard json-encode calls require exactly one argument",
        ));
    };
    if argument.parameter() != parameter {
        return Err(argument_error(
            Some(argument.parameter()),
            "standard json-encode arguments must bind the pinned parameter identity",
        ));
    }
    match argument.value() {
        RuntimeValue::Null(_) => Err(argument_error(
            Some(parameter),
            "standard json-encode arguments cannot be NULL",
        )),
        value => Ok(value),
    }
}

/// Converts one bound runtime value to JSON without loss.
///
/// The closed ADR 0057 conversion matrix accepts exactly: null, booleans,
/// integers, bigints, floats, text, bytes (base64), references (an explicit
/// `{"$ref": "orna://<type-name>/<object-id>", "$type": "<type-name>"}`
/// object), lists (arrays), maps (objects), and canonical `std.json.Value`
/// payloads. Every other runtime form (enums, records, other opaque values,
/// options, and invocation carriers) cannot be represented without loss and
/// is rejected.
pub(super) fn encode_json_value(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
) -> Result<serde_json::Value, &'static str> {
    match value {
        RuntimeValue::Null(_) => Ok(serde_json::Value::Null),
        RuntimeValue::Boolean(value) => Ok(serde_json::Value::Bool(*value)),
        RuntimeValue::Integer(value) => Ok(serde_json::Value::from(*value)),
        RuntimeValue::BigInt(value) => Ok(serde_json::Value::from(*value)),
        RuntimeValue::Float(value) => serde_json::Number::from_f64(value.value())
            .map(serde_json::Value::Number)
            .ok_or("std.json.encode cannot represent a non-finite FLOAT value"),
        RuntimeValue::Text(value) => Ok(serde_json::Value::String(value.clone())),
        RuntimeValue::Bytes(value) => Ok(serde_json::Value::String(BASE64_STANDARD.encode(value))),
        RuntimeValue::Reference { target, object } => {
            let Some(definition) = active.catalogue().object_type_by_id(*target) else {
                return Err(
                    "std.json.encode cannot encode a reference outside the active catalogue",
                );
            };
            let type_name = definition.name().to_string();
            Ok(serde_json::json!({
                "$ref": format!("orna://{type_name}/{}", object.canonical()),
                "$type": type_name,
            }))
        }
        RuntimeValue::Constructed(value) => match value.kind() {
            ConstructedValueKind::List(values) => values
                .iter()
                .map(|value| encode_json_value(active, value))
                .collect::<Result<Vec<_>, _>>()
                .map(serde_json::Value::Array),
            ConstructedValueKind::Map(entries) => entries
                .iter()
                .map(|(key, value)| {
                    Ok((
                        encode_json_object_key(active, key)?,
                        encode_json_value(active, value)?,
                    ))
                })
                .collect::<Result<serde_json::Map<String, serde_json::Value>, _>>()
                .map(serde_json::Value::Object),
            ConstructedValueKind::Option(_) => {
                Err("std.json.encode cannot convert an OPTION value to JSON without loss")
            }
            _ => Err(
                "std.json.encode cannot convert an unknown constructed value to JSON without loss",
            ),
        },
        RuntimeValue::Enum(_) => {
            Err("std.json.encode cannot convert an ENUM value to JSON without loss")
        }
        RuntimeValue::Record(_) => {
            Err("std.json.encode cannot convert a RECORD value to JSON without loss")
        }
        RuntimeValue::Opaque(value) if value.opaque_type() == STD_JSON_VALUE_TYPE_ID => {
            decode_std_json_value(value)
        }
        RuntimeValue::Opaque(_) => {
            Err("std.json.encode cannot convert an OPAQUE value to JSON without loss")
        }
        RuntimeValue::InvokeValue(_)
        | RuntimeValue::InvokeRequest(_)
        | RuntimeValue::InvokeEvent(_) => {
            Err("std.json.encode cannot convert an invocation carrier to JSON without loss")
        }
        _ => Err("std.json.encode cannot convert an unknown runtime value to JSON without loss"),
    }
}

/// Decodes the already-validated canonical payload of one `std.json.Value`.
fn decode_std_json_value(value: &OpaqueValue) -> Result<serde_json::Value, &'static str> {
    let payload = value.canonical_payload();
    let magic = JSON_MAGIC.as_bytes();
    if !payload.starts_with(magic) {
        return Err("std.json.encode cannot decode a canonical std.json.Value payload");
    }
    let length_start = magic.len();
    let body_start = length_start
        .checked_add(4)
        .ok_or("std.json.encode cannot decode a canonical std.json.Value payload")?;
    let length_bytes = payload
        .get(length_start..body_start)
        .ok_or("std.json.encode cannot decode a canonical std.json.Value payload")?;
    let body_length = u32::from_be_bytes(
        length_bytes
            .try_into()
            .map_err(|_| "std.json.encode cannot decode a canonical std.json.Value payload")?,
    ) as usize;
    let body = payload
        .get(body_start..)
        .filter(|body| body.len() == body_length)
        .ok_or("std.json.encode cannot decode a canonical std.json.Value payload")?;
    serde_json::from_slice(body)
        .map_err(|_| "std.json.encode cannot decode a canonical std.json.Value payload")
}

/// Converts one map key to its canonical JSON object-key text.
///
/// JSON object keys are strings, so each lossless scalar form renders in its
/// canonical text: text verbatim, booleans and numbers in decimal, bytes as
/// base64, enums as their declared label, and references as their canonical
/// `orna://` URI. Every other form cannot be reduced to a JSON string key
/// without loss and is rejected.
fn encode_json_object_key(
    active: &ActiveDatabaseRevision,
    key: &RuntimeValue,
) -> Result<String, &'static str> {
    match key {
        RuntimeValue::Text(value) => Ok(value.clone()),
        RuntimeValue::Boolean(value) => Ok(value.to_string()),
        RuntimeValue::Integer(value) => Ok(value.to_string()),
        RuntimeValue::BigInt(value) => Ok(value.to_string()),
        RuntimeValue::Float(value) => Ok(value.value().to_string()),
        RuntimeValue::Bytes(value) => Ok(BASE64_STANDARD.encode(value)),
        RuntimeValue::Enum(value) => Ok(value.label().to_owned()),
        RuntimeValue::Reference { target, object } => {
            let Some(definition) = active.catalogue().object_type_by_id(*target) else {
                return Err(
                    "std.json.encode cannot encode a reference outside the active catalogue",
                );
            };
            Ok(format!(
                "orna://{}/{}",
                definition.name(),
                object.canonical()
            ))
        }
        _ => Err("std.json.encode map keys must be losslessly encodable JSON strings"),
    }
}

/// Frames one media-typed byte payload as the canonical ADR 0058
/// `std.io.ByteStream` payload: `ORNA-BYTE-STREAM/1 <media-type-len:u32 be>
/// <media-type> <len:u32 be> <bytes>`.
pub(super) fn frame_byte_stream(media_type: &[u8], bytes: &[u8]) -> Vec<u8> {
    let mut payload =
        Vec::with_capacity(BYTE_STREAM_MAGIC.len() + 4 + media_type.len() + 4 + bytes.len());
    payload.extend_from_slice(BYTE_STREAM_MAGIC.as_bytes());
    payload.extend_from_slice(
        &u32::try_from(media_type.len())
            .expect("the presenter media type length fits u32")
            .to_be_bytes(),
    );
    payload.extend_from_slice(media_type);
    payload.extend_from_slice(
        &u32::try_from(bytes.len())
            .expect("the presenter byte payload length fits u32")
            .to_be_bytes(),
    );
    payload.extend_from_slice(bytes);
    payload
}

/// Frames one UTF-8 document as the canonical ADR 0058 `std.terminal.Document`
/// payload: `ORNA-TERMINAL-DOCUMENT/1 <len:u32 be> <utf-8 bytes>`.
pub(super) fn frame_terminal_document(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut payload = Vec::with_capacity(TERMINAL_DOCUMENT_MAGIC.len() + 4 + bytes.len());
    payload.extend_from_slice(TERMINAL_DOCUMENT_MAGIC.as_bytes());
    payload.extend_from_slice(
        &u32::try_from(bytes.len())
            .expect("the presenter document length fits u32")
            .to_be_bytes(),
    );
    payload.extend_from_slice(bytes);
    payload
}

/// Renders one validated [`ResultRows`] as the fixed plain-text terminal
/// table.
///
/// The fixed layout is one header line (column names padded to their column
/// width), one separator line (`-` repeated to the width of each column), one
/// line per row (cells padded to their column width), and a trailing row
/// count line. Columns are joined by a single space, every line ends with
/// `\n`, and the document carries no control characters: any rendered cell or
/// column name containing a control character is rejected.
pub(super) fn render_terminal_table(
    active: &ActiveDatabaseRevision,
    rows: &ResultRows,
) -> Result<String, &'static str> {
    let columns = rows.columns();
    let mut widths = Vec::with_capacity(columns.len());
    let mut header = Vec::with_capacity(columns.len());
    for column in columns {
        reject_control_characters(
            column.name(),
            "terminal table column names cannot contain control characters",
        )?;
        widths.push(column.name().chars().count());
        header.push(column.name().to_owned());
    }
    let mut body = Vec::with_capacity(rows.rows().len());
    for row in rows.rows() {
        let mut cells = Vec::with_capacity(columns.len());
        for (index, value) in row.values().iter().enumerate() {
            let cell = render_terminal_cell(active, value)?;
            let width = cell.chars().count();
            if width > widths[index] {
                widths[index] = width;
            }
            cells.push(cell);
        }
        body.push(cells);
    }
    let mut document = String::new();
    push_table_line(&mut document, &header, &widths, false);
    push_table_line(&mut document, &header, &widths, true);
    for cells in &body {
        push_table_line(&mut document, cells, &widths, false);
    }
    let count = rows.rows().len();
    if count == 1 {
        document.push_str("(1 row)\n");
    } else {
        document.push_str(&format!("({count} rows)\n"));
    }
    Ok(document)
}

/// Renders one validated [`ResultRows`] as one CSV document.
///
/// The fixed layout is one header row of column names followed by one row
/// per result row; every row ends with `\n` and the document ends with a
/// trailing newline. Cells render with the same closed value rules as the
/// terminal table, then receive RFC-4180-style quoting: a cell containing a
/// comma, double quote, CR, or LF is quoted and embedded quotes are doubled.
/// NUL and other forbidden control characters remain rejected, while CR and LF
/// are valid CSV data.
pub(super) fn render_csv_document(
    active: &ActiveDatabaseRevision,
    rows: &ResultRows,
) -> Result<String, &'static str> {
    let columns = rows.columns();
    let mut document = String::new();
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            document.push(',');
        }
        reject_csv_control_characters(
            column.name(),
            "csv column names cannot contain control characters",
        )?;
        push_csv_field(&mut document, column.name());
    }
    document.push('\n');
    for row in rows.rows() {
        for (index, value) in row.values().iter().enumerate() {
            if index > 0 {
                document.push(',');
            }
            let cell = render_csv_cell(active, value)?;
            push_csv_field(&mut document, &cell);
        }
        document.push('\n');
    }
    Ok(document)
}

/// Appends one CSV field to the document with RFC-4180-style quoting.
///
/// A field containing a comma, double quote, CR, or LF is wrapped in double
/// quotes and every embedded double quote is doubled. A field free of those
/// four characters is appended verbatim.
fn push_csv_field(document: &mut String, field: &str) {
    let needs_quoting = field
        .chars()
        .any(|character| matches!(character, ',' | '"' | '\r' | '\n'));
    if !needs_quoting {
        document.push_str(field);
        return;
    }
    document.push('"');
    for character in field.chars() {
        if character == '"' {
            document.push('"');
        }
        document.push(character);
    }
    document.push('"');
}

/// Appends one aligned table line to the document.
///
/// Data lines left-pad every cell to its column width (the final column is
/// not padded, so lines carry no trailing whitespace); the separator line
/// repeats `-` to the width of each column. Columns are joined by one space.
fn push_table_line(document: &mut String, cells: &[String], widths: &[usize], separator: bool) {
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            document.push(' ');
        }
        if separator {
            document.extend(std::iter::repeat_n('-', widths[index]));
        } else {
            document.push_str(cell);
            if index + 1 < cells.len() {
                let width = cell.chars().count();
                document.extend(std::iter::repeat_n(' ', widths[index] - width));
            }
        }
    }
    document.push('\n');
}

/// Renders one terminal-table cell as plain text.
///
/// Nulls render as `NULL`, scalars in their canonical text, bytes as base64,
/// references as their canonical object id, enums as their declared label,
/// and records as `type-name{field=value, ...}` in declaration order. Opaque
/// values, constructed values, and invocation carriers cannot appear in a
/// validated [`ResultRows`]; the explicit arms keep the renderer closed.
fn render_terminal_cell(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
) -> Result<String, &'static str> {
    render_cell(active, value, reject_control_characters)
}

/// Renders one CSV cell, allowing RFC-4180 CR/LF data while rejecting other
/// forbidden control characters.
fn render_csv_cell(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
) -> Result<String, &'static str> {
    render_cell(active, value, reject_csv_control_characters)
}

fn render_cell(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
    validate: fn(&str, &'static str) -> Result<(), &'static str>,
) -> Result<String, &'static str> {
    let cell = match value {
        RuntimeValue::Null(_) => "NULL".to_owned(),
        RuntimeValue::Boolean(value) => value.to_string(),
        RuntimeValue::Integer(value) => value.to_string(),
        RuntimeValue::BigInt(value) => value.to_string(),
        RuntimeValue::Float(value) => value.value().to_string(),
        RuntimeValue::Text(value) => value.clone(),
        RuntimeValue::Bytes(value) => BASE64_STANDARD.encode(value),
        RuntimeValue::Reference { object, .. } => object.canonical(),
        RuntimeValue::Enum(value) => value.label().to_owned(),
        RuntimeValue::Record(value) => render_record_cell_with(active, value, validate)?,
        RuntimeValue::Opaque(_) => {
            return Err("terminal tables cannot render OPAQUE values");
        }
        RuntimeValue::Constructed(_) => {
            return Err("terminal tables cannot render constructed values");
        }
        RuntimeValue::InvokeValue(_)
        | RuntimeValue::InvokeRequest(_)
        | RuntimeValue::InvokeEvent(_) => {
            return Err("terminal tables cannot render invocation carriers");
        }
        _ => return Err("terminal tables cannot render an unknown runtime value"),
    };
    validate(
        &cell,
        "terminal table cells cannot contain control characters",
    )?;
    Ok(cell)
}

/// Renders one record cell as `type-name{field=value, ...}`.
///
/// Field names and the record type name come from the active catalogue;
/// field values render with the same closed cell rules and are never null
/// (the record constructor rejects null fields).
fn render_record_cell_with(
    active: &ActiveDatabaseRevision,
    record: &RecordValue,
    validate: fn(&str, &'static str) -> Result<(), &'static str>,
) -> Result<String, &'static str> {
    let Some(definition) = active
        .catalogue()
        .record_value_type_by_id(record.record_type())
    else {
        return Err("terminal tables cannot render a record outside the active catalogue");
    };
    let mut cell = definition.name().to_string();
    cell.push('{');
    for (index, (field, value)) in definition.fields().iter().zip(record.fields()).enumerate() {
        if index > 0 {
            cell.push_str(", ");
        }
        cell.push_str(field.name());
        cell.push('=');
        cell.push_str(&render_cell(active, value, validate)?);
    }
    cell.push('}');
    Ok(cell)
}

/// Rejects any control character in one rendered table text fragment.
fn reject_control_characters(text: &str, rule: &'static str) -> Result<(), &'static str> {
    if text.chars().any(char::is_control) {
        Err(rule)
    } else {
        Ok(())
    }
}

/// Rejects controls that cannot be represented as CSV data. CR and LF are
/// explicitly allowed because RFC-4180 quoting preserves them inside fields.
fn reject_csv_control_characters(text: &str, rule: &'static str) -> Result<(), &'static str> {
    if text
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\r' | '\n'))
    {
        Err(rule)
    } else {
        Ok(())
    }
}
