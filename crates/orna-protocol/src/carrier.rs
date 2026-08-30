use super::*;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct TextKey(Vec<u8>);

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DescriptorKey(Vec<u8>);

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SinkKey {
    descriptor: DescriptorKey,
    media_types: Vec<u8>,
    streaming: u8,
    preference_rank: [u8; 4],
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ContractKey {
    name: TextKey,
    version: TextKey,
    features: Vec<u8>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RuntimeKey {
    name: TextKey,
    version: TextKey,
    remaining: Vec<u8>,
}

struct CarrierWriter {
    bytes: Vec<u8>,
}

impl CarrierWriter {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }

    fn append(&mut self, bytes: &[u8]) -> Result<(), InvocationCarrierCodecError> {
        let actual = self.bytes.len().checked_add(bytes.len()).ok_or(
            InvocationCarrierCodecError::PayloadTooLarge {
                actual: usize::MAX,
                maximum: PAYLOAD_LIMIT,
            },
        )?;
        if actual > PAYLOAD_LIMIT {
            return Err(InvocationCarrierCodecError::PayloadTooLarge {
                actual,
                maximum: PAYLOAD_LIMIT,
            });
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn u8(&mut self, value: u8) -> Result<(), InvocationCarrierCodecError> {
        self.append(&[value])
    }

    fn u16(&mut self, value: u16) -> Result<(), InvocationCarrierCodecError> {
        self.append(&value.to_be_bytes())
    }

    fn u32(&mut self, value: u32) -> Result<(), InvocationCarrierCodecError> {
        self.append(&value.to_be_bytes())
    }

    fn u64(&mut self, value: u64) -> Result<(), InvocationCarrierCodecError> {
        self.append(&value.to_be_bytes())
    }

    fn i32(&mut self, value: i32) -> Result<(), InvocationCarrierCodecError> {
        self.append(&value.to_be_bytes())
    }

    fn count(&mut self, count: usize) -> Result<(), InvocationCarrierCodecError> {
        let count =
            u32::try_from(count).map_err(|_| InvocationCarrierCodecError::PayloadTooLarge {
                actual: usize::MAX,
                maximum: PAYLOAD_LIMIT,
            })?;
        self.u32(count)
    }

    fn text(&mut self, value: &str) -> Result<(), InvocationCarrierCodecError> {
        self.length_prefixed(value.as_bytes())
    }

    fn length_prefixed(&mut self, value: &[u8]) -> Result<(), InvocationCarrierCodecError> {
        let length = u32::try_from(value.len()).map_err(|_| {
            InvocationCarrierCodecError::PayloadTooLarge {
                actual: value.len(),
                maximum: PAYLOAD_LIMIT,
            }
        })?;
        self.u32(length)?;
        self.append(value)
    }
}

pub(super) fn encode_invocation_carrier(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: &RuntimeValue,
) -> Result<Vec<u8>, InvocationCarrierCodecError> {
    let (carrier, payload) = match value {
        RuntimeValue::InvokeValue(value) => (
            SYS_INVOKE_VALUE_TYPE_ID,
            encode_invoke_value_payload(
                active,
                registry,
                value,
                InvocationCarrierPath::one(InvocationCarrierPathSegment::ValueInner),
            )?,
        ),
        RuntimeValue::InvokeRequest(request) => (
            SYS_INVOKE_REQUEST_TYPE_ID,
            encode_invoke_request_payload(active, registry, request)?,
        ),
        RuntimeValue::InvokeEvent(event) => (
            SYS_INVOKE_EVENT_TYPE_ID,
            encode_invoke_event_payload(active, registry, event)?,
        ),
        _ => unreachable!("carrier classification and runtime variant must agree"),
    };
    let validated = parse_invocation_carrier(carrier, &payload)?;
    validated.preflight(carrier)?;
    Ok(encode_with_marker(
        CONSTRUCTED_MARKER,
        OPAQUE_TAG,
        carrier,
        &payload,
    ))
}

fn encode_invoke_value_payload(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: &InvokeValue,
    path: InvocationCarrierPath,
) -> Result<Vec<u8>, InvocationCarrierCodecError> {
    if let Some(carrier) = invocation_carrier_type_id(value.value()) {
        return Err(InvocationCarrierCodecError::NestedCarrier { path, carrier });
    }
    let encoded = encode_orv5_value(active, registry, value.value()).map_err(|source| {
        InvocationCarrierCodecError::InnerValue {
            path,
            source: Box::new(source),
        }
    })?;
    let mut writer = CarrierWriter::new();
    writer.u8(1)?;
    writer.length_prefixed(&encoded)?;
    Ok(writer.finish())
}

fn encode_embedded_invoke_value(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: &InvokeValue,
    path: InvocationCarrierPath,
) -> Result<Vec<u8>, InvocationCarrierCodecError> {
    let payload = encode_invoke_value_payload(active, registry, value, path)?;
    Ok(encode_with_marker(
        CONSTRUCTED_MARKER,
        OPAQUE_TAG,
        SYS_INVOKE_VALUE_TYPE_ID,
        &payload,
    ))
}

fn append_embedded_invoke_value(
    writer: &mut CarrierWriter,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: &InvokeValue,
    path: InvocationCarrierPath,
) -> Result<(), InvocationCarrierCodecError> {
    let encoded = encode_embedded_invoke_value(active, registry, value, path)?;
    writer.length_prefixed(&encoded)
}

fn append_optional_invoke_value(
    writer: &mut CarrierWriter,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: Option<&InvokeValue>,
    path: InvocationCarrierPath,
) -> Result<(), InvocationCarrierCodecError> {
    match value {
        Some(value) => {
            writer.u8(1)?;
            append_embedded_invoke_value(writer, active, registry, value, path)
        }
        None => writer.u8(0),
    }
}

fn append_optional_text(
    writer: &mut CarrierWriter,
    value: Option<&str>,
) -> Result<(), InvocationCarrierCodecError> {
    match value {
        Some(value) => {
            writer.u8(1)?;
            writer.text(value)
        }
        None => writer.u8(0),
    }
}

fn append_optional_id<T>(
    writer: &mut CarrierWriter,
    value: Option<T>,
    bytes: impl FnOnce(T) -> [u8; 16],
) -> Result<(), InvocationCarrierCodecError> {
    match value {
        Some(value) => {
            writer.u8(1)?;
            writer.append(&bytes(value))
        }
        None => writer.u8(0),
    }
}

fn append_semantic_name(
    writer: &mut CarrierWriter,
    name: &QualifiedSemanticName,
) -> Result<(), InvocationCarrierCodecError> {
    writer.count(name.parts().len())?;
    for part in name.parts() {
        writer.text(part)?;
    }
    Ok(())
}

fn encoded_descriptor(
    descriptor: &TypeDescriptor,
    path: InvocationCarrierPath,
) -> Result<Vec<u8>, InvocationCarrierCodecError> {
    reject_carrier_descriptor(descriptor, &path)?;
    let mut encoded = Vec::new();
    encode_constructed_descriptor(descriptor, &mut encoded)
        .map_err(|_| InvocationCarrierCodecError::InvalidField { path })?;
    Ok(encoded)
}

fn append_descriptor_bytes(
    writer: &mut CarrierWriter,
    encoded: &[u8],
) -> Result<(), InvocationCarrierCodecError> {
    let length =
        u16::try_from(encoded.len()).map_err(|_| InvocationCarrierCodecError::PayloadTooLarge {
            actual: encoded.len(),
            maximum: PAYLOAD_LIMIT,
        })?;
    writer.u16(length)?;
    writer.append(encoded)
}

fn reject_carrier_descriptor(
    descriptor: &TypeDescriptor,
    path: &InvocationCarrierPath,
) -> Result<(), InvocationCarrierCodecError> {
    match descriptor.kind() {
        TypeDescriptorKind::Named(type_id) | TypeDescriptorKind::Reference(type_id) => {
            if invocation_carrier_by_id(type_id).is_some() {
                Err(InvocationCarrierCodecError::NestedCarrier {
                    path: path.clone(),
                    carrier: type_id,
                })
            } else {
                Ok(())
            }
        }
        TypeDescriptorKind::List(child) | TypeDescriptorKind::Option(child) => {
            reject_carrier_descriptor(child, path)
        }
        TypeDescriptorKind::Map { key, value } => {
            reject_carrier_descriptor(key, path)?;
            reject_carrier_descriptor(value, path)
        }
        TypeDescriptorKind::Set(child) => {
            if !matches!(
                child.kind(),
                TypeDescriptorKind::Named(_) | TypeDescriptorKind::Reference(_)
            ) {
                return Err(InvocationCarrierCodecError::InvalidField { path: path.clone() });
            }
            reject_carrier_descriptor(child, path)
        }
        TypeDescriptorKind::Stream(_) => {
            Err(InvocationCarrierCodecError::InvalidField { path: path.clone() })
        }
    }
}

fn insert_canonical<K: Ord>(
    items: &mut BTreeMap<K, (usize, Vec<u8>)>,
    key: K,
    index: usize,
    encoded: Vec<u8>,
    path: &InvocationCarrierPath,
) -> Result<(), InvocationCarrierCodecError> {
    if let Some((first, _)) = items.get(&key) {
        return Err(InvocationCarrierCodecError::DuplicateItem {
            path: path.clone(),
            first: *first,
            duplicate: index,
        });
    }
    items.insert(key, (index, encoded));
    Ok(())
}

fn add_prepared_size(
    total: &mut usize,
    additional: usize,
) -> Result<(), InvocationCarrierCodecError> {
    let actual =
        total
            .checked_add(additional)
            .ok_or(InvocationCarrierCodecError::PayloadTooLarge {
                actual: usize::MAX,
                maximum: PAYLOAD_LIMIT,
            })?;
    if actual > PAYLOAD_LIMIT {
        return Err(InvocationCarrierCodecError::PayloadTooLarge {
            actual,
            maximum: PAYLOAD_LIMIT,
        });
    }
    *total = actual;
    Ok(())
}

fn canonical_text_list(
    values: &[String],
    path: &InvocationCarrierPath,
) -> Result<Vec<u8>, InvocationCarrierCodecError> {
    let mut canonical = BTreeMap::new();
    let mut prepared_size = 4;
    for (index, value) in values.iter().enumerate() {
        if value.is_empty() {
            return Err(InvocationCarrierCodecError::InvalidField { path: path.clone() });
        }
        let mut encoded = CarrierWriter::new();
        encoded.text(value)?;
        let encoded = encoded.finish();
        add_prepared_size(&mut prepared_size, encoded.len())?;
        insert_canonical(
            &mut canonical,
            TextKey(value.as_bytes().to_vec()),
            index,
            encoded,
            path,
        )?;
    }
    let mut writer = CarrierWriter::new();
    writer.count(canonical.len())?;
    for (_, encoded) in canonical.into_values() {
        writer.append(&encoded)?;
    }
    Ok(writer.finish())
}

fn canonical_descriptor_list(
    values: &[TypeDescriptor],
    path: &InvocationCarrierPath,
) -> Result<Vec<u8>, InvocationCarrierCodecError> {
    let mut canonical = BTreeMap::new();
    let mut prepared_size = 4;
    for (index, value) in values.iter().enumerate() {
        let item_path = path.with(InvocationCarrierPathSegment::ConsumedType(index));
        let descriptor = encoded_descriptor(value, item_path)?;
        let mut encoded = CarrierWriter::new();
        append_descriptor_bytes(&mut encoded, &descriptor)?;
        let encoded = encoded.finish();
        add_prepared_size(&mut prepared_size, encoded.len())?;
        insert_canonical(
            &mut canonical,
            DescriptorKey(descriptor),
            index,
            encoded,
            path,
        )?;
    }
    let mut writer = CarrierWriter::new();
    writer.count(canonical.len())?;
    for (_, encoded) in canonical.into_values() {
        writer.append(&encoded)?;
    }
    Ok(writer.finish())
}

fn canonical_contract_list(
    values: &[InvocationRuntimeContract],
    path: &InvocationCarrierPath,
) -> Result<Vec<u8>, InvocationCarrierCodecError> {
    let mut canonical = BTreeMap::new();
    let mut prepared_size = 4;
    for (index, value) in values.iter().enumerate() {
        let item_path = path.with(InvocationCarrierPathSegment::Contract(index));
        if value.name().is_empty() || value.version().is_empty() {
            return Err(InvocationCarrierCodecError::InvalidField { path: item_path });
        }
        let features_path = item_path.with(InvocationCarrierPathSegment::Features);
        let features = canonical_text_list(value.features(), &features_path)?;
        let mut encoded = CarrierWriter::new();
        encoded.text(value.name())?;
        encoded.text(value.version())?;
        encoded.append(&features)?;
        let encoded = encoded.finish();
        add_prepared_size(&mut prepared_size, encoded.len())?;
        insert_canonical(
            &mut canonical,
            ContractKey {
                name: TextKey(value.name().as_bytes().to_vec()),
                version: TextKey(value.version().as_bytes().to_vec()),
                features,
            },
            index,
            encoded,
            path,
        )?;
    }
    let mut writer = CarrierWriter::new();
    writer.count(canonical.len())?;
    for (_, encoded) in canonical.into_values() {
        writer.append(&encoded)?;
    }
    Ok(writer.finish())
}

fn canonical_sink_list(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    values: &[InvocationSinkOffer],
    path: &InvocationCarrierPath,
) -> Result<Vec<u8>, InvocationCarrierCodecError> {
    let mut canonical = BTreeMap::new();
    let mut prepared_size = 4;
    for (index, value) in values.iter().enumerate() {
        let item_path = path.with(InvocationCarrierPathSegment::Sink(index));
        let descriptor_path = item_path.with(InvocationCarrierPathSegment::Descriptor);
        let descriptor = encoded_descriptor(value.descriptor(), descriptor_path)?;
        let media_types_path = item_path.with(InvocationCarrierPathSegment::MediaTypes);
        let media_types = canonical_text_list(value.media_types(), &media_types_path)?;
        let mut encoded = CarrierWriter::new();
        append_descriptor_bytes(&mut encoded, &descriptor)?;
        encoded.append(&media_types)?;
        encoded.u8(u8::from(value.streaming()))?;
        encoded.i32(value.preference_rank())?;
        append_optional_invoke_value(
            &mut encoded,
            active,
            registry,
            value.limits(),
            item_path.with(InvocationCarrierPathSegment::Limits),
        )?;
        let encoded = encoded.finish();
        add_prepared_size(&mut prepared_size, encoded.len())?;
        insert_canonical(
            &mut canonical,
            SinkKey {
                descriptor: DescriptorKey(descriptor),
                media_types,
                streaming: u8::from(value.streaming()),
                preference_rank: value.preference_rank().to_be_bytes(),
            },
            index,
            encoded,
            path,
        )?;
    }
    let mut writer = CarrierWriter::new();
    writer.count(canonical.len())?;
    for (_, encoded) in canonical.into_values() {
        writer.append(&encoded)?;
    }
    Ok(writer.finish())
}

fn canonical_runtime_list(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    values: &[InvocationRuntimeOffer],
    path: &InvocationCarrierPath,
) -> Result<Vec<u8>, InvocationCarrierCodecError> {
    let mut canonical = BTreeMap::new();
    let mut prepared_size = 4;
    for (index, value) in values.iter().enumerate() {
        let item_path = path.with(InvocationCarrierPathSegment::Runtime(index));
        if value.name().is_empty() || value.version().is_empty() {
            return Err(InvocationCarrierCodecError::InvalidField { path: item_path });
        }
        let consumed_path = item_path.with(InvocationCarrierPathSegment::ConsumedTypes);
        let consumed = canonical_descriptor_list(value.consumed_descriptors(), &consumed_path)?;
        let contracts_path = item_path.with(InvocationCarrierPathSegment::Contracts);
        let contracts = canonical_contract_list(value.contracts(), &contracts_path)?;
        let mut remaining = CarrierWriter::new();
        remaining.append(&consumed)?;
        remaining.append(&contracts)?;
        remaining.i32(value.preference_rank())?;
        remaining.u8(u8::from(value.trusted()))?;
        append_optional_invoke_value(
            &mut remaining,
            active,
            registry,
            value.limits(),
            item_path.with(InvocationCarrierPathSegment::Limits),
        )?;
        let remaining = remaining.finish();
        let mut encoded = CarrierWriter::new();
        encoded.text(value.name())?;
        encoded.text(value.version())?;
        encoded.append(&remaining)?;
        let encoded = encoded.finish();
        add_prepared_size(&mut prepared_size, encoded.len())?;
        insert_canonical(
            &mut canonical,
            RuntimeKey {
                name: TextKey(value.name().as_bytes().to_vec()),
                version: TextKey(value.version().as_bytes().to_vec()),
                remaining,
            },
            index,
            encoded,
            path,
        )?;
    }
    let mut writer = CarrierWriter::new();
    writer.count(canonical.len())?;
    for (_, encoded) in canonical.into_values() {
        writer.append(&encoded)?;
    }
    Ok(writer.finish())
}

fn encode_invoke_request_payload(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    request: &InvokeRequest,
) -> Result<Vec<u8>, InvocationCarrierCodecError> {
    if request.node_count() > MAX_INVOCATION_CARRIER_NODES {
        return Err(InvocationCarrierCodecError::TooManyNodes {
            maximum: MAX_INVOCATION_CARRIER_NODES,
        });
    }
    let mut writer = CarrierWriter::new();
    writer.u8(1)?;
    match request.target() {
        InvocationTarget::FunctionId(function) => {
            writer.u8(0)?;
            writer.append(&function.to_bytes())?;
        }
        InvocationTarget::QualifiedName(name) => {
            writer.u8(1)?;
            append_semantic_name(&mut writer, name)?;
        }
        _ => {
            return Err(InvocationCarrierCodecError::InvalidField {
                path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestTarget),
            });
        }
    }
    writer.count(request.arguments().len())?;
    for (index, argument) in request.arguments().iter().enumerate() {
        let argument_path =
            InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestArguments)
                .with(InvocationCarrierPathSegment::Argument(index));
        match argument.selector() {
            InvocationParameterSelector::ParameterId(parameter) => {
                writer.u8(0)?;
                writer.append(&parameter.to_bytes())?;
            }
            InvocationParameterSelector::Name(name) => {
                if name.is_empty() {
                    return Err(InvocationCarrierCodecError::InvalidField {
                        path: argument_path.with(InvocationCarrierPathSegment::Selector),
                    });
                }
                writer.u8(1)?;
                writer.text(name)?;
            }
            _ => {
                return Err(InvocationCarrierCodecError::InvalidField {
                    path: argument_path.with(InvocationCarrierPathSegment::Selector),
                });
            }
        }
        append_embedded_invoke_value(
            &mut writer,
            active,
            registry,
            argument.value(),
            argument_path.with(InvocationCarrierPathSegment::Value),
        )?;
    }
    append_caller_context(&mut writer, active, registry, request.caller_context())?;
    append_client_offer(&mut writer, active, registry, request.client_offer())?;
    append_output_requirement(&mut writer, request.output_requirement())?;
    append_optional_text(&mut writer, request.state_profile())?;
    writer.u8(trace_policy_discriminant(request.trace_policy())?)?;
    writer.u8(0)?;
    match request.idempotency_key() {
        Some(key) => {
            if key.is_empty() {
                return Err(InvocationCarrierCodecError::InvalidField {
                    path: InvocationCarrierPath::one(
                        InvocationCarrierPathSegment::RequestIdempotencyKey,
                    ),
                });
            }
            writer.u8(1)?;
            writer.length_prefixed(key)?;
        }
        None => writer.u8(0)?,
    }
    append_optional_id(
        &mut writer,
        request.parent_invocation_id(),
        InvocationId::to_bytes,
    )?;
    append_optional_invoke_value(
        &mut writer,
        active,
        registry,
        request.observer_context(),
        InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestObserverContext),
    )?;
    Ok(writer.finish())
}

fn append_caller_context(
    writer: &mut CarrierWriter,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    caller: &InvocationCallerContext,
) -> Result<(), InvocationCarrierCodecError> {
    writer.u8(caller_kind_discriminant(caller.kind())?)?;
    writer.u8(u8::from(caller.interactive()) | (u8::from(caller.stdout_is_tty()) << 1))?;
    append_optional_u32(writer, caller.terminal_columns())?;
    append_optional_u32(writer, caller.terminal_rows())?;
    writer.text(caller.locale())?;
    writer.text(caller.timezone())?;
    append_optional_invoke_value(
        writer,
        active,
        registry,
        caller.preference_policy(),
        InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestCaller)
            .with(InvocationCarrierPathSegment::PreferencePolicy),
    )
}

fn append_optional_u32(
    writer: &mut CarrierWriter,
    value: Option<u32>,
) -> Result<(), InvocationCarrierCodecError> {
    match value {
        Some(value) => {
            writer.u8(1)?;
            writer.u32(value)
        }
        None => writer.u8(0),
    }
}

fn append_client_offer(
    writer: &mut CarrierWriter,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    offer: &InvocationClientOffer,
) -> Result<(), InvocationCarrierCodecError> {
    writer.u16(offer.protocol_major())?;
    writer.text(offer.locale())?;
    writer.text(offer.timezone())?;
    let sinks_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestClientOffer)
        .with(InvocationCarrierPathSegment::ClientSinks);
    writer.append(&canonical_sink_list(
        active,
        registry,
        offer.sink_offers(),
        &sinks_path,
    )?)?;
    let runtimes_path =
        InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestClientOffer)
            .with(InvocationCarrierPathSegment::ClientRuntimes);
    writer.append(&canonical_runtime_list(
        active,
        registry,
        offer.runtime_offers(),
        &runtimes_path,
    )?)?;
    writer.u32(offer.maximum_frame_size())?;
    writer.u64(offer.maximum_artifact_size())?;
    append_optional_invoke_value(
        writer,
        active,
        registry,
        offer.limits(),
        InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestClientOffer)
            .with(InvocationCarrierPathSegment::ClientLimits),
    )?;
    append_optional_invoke_value(
        writer,
        active,
        registry,
        offer.preferences(),
        InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestClientOffer)
            .with(InvocationCarrierPathSegment::ClientPreferences),
    )
}

fn append_output_requirement(
    writer: &mut CarrierWriter,
    output: Option<&InvocationOutputRequirement>,
) -> Result<(), InvocationCarrierCodecError> {
    let Some(output) = output else {
        return writer.u8(0);
    };
    writer.u8(1)?;
    append_optional_text(writer, output.alias())?;
    append_optional_text(writer, output.media_type())?;
    match output.type_selector() {
        Some(InvocationOutputTypeSelector::TypeId(type_id)) => {
            writer.u8(1)?;
            writer.u8(0)?;
            writer.append(&type_id.to_bytes())?;
        }
        Some(InvocationOutputTypeSelector::QualifiedName(name)) => {
            writer.u8(1)?;
            writer.u8(1)?;
            append_semantic_name(writer, name)?;
        }
        Some(_) => {
            return Err(InvocationCarrierCodecError::InvalidField {
                path: InvocationCarrierPath::one(
                    InvocationCarrierPathSegment::RequestOutputRequirement,
                )
                .with(InvocationCarrierPathSegment::OutputType),
            });
        }
        None => writer.u8(0)?,
    }
    writer.u8(streaming_requirement_discriminant(output.streaming())?)
}

fn caller_kind_discriminant(kind: InvocationCallerKind) -> Result<u8, InvocationCarrierCodecError> {
    Ok(match kind {
        InvocationCallerKind::CliTty => 0,
        InvocationCallerKind::CliPipe => 1,
        InvocationCallerKind::DesktopLauncher => 2,
        InvocationCallerKind::Browser => 3,
        InvocationCallerKind::ClientFunction => 4,
        InvocationCallerKind::JsonRpcGateway => 5,
        InvocationCallerKind::McpGateway => 6,
        InvocationCallerKind::Scheduler => 7,
        InvocationCallerKind::TestRunner => 8,
        InvocationCallerKind::Recovery => 9,
        _ => {
            return Err(InvocationCarrierCodecError::InvalidField {
                path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestCaller)
                    .with(InvocationCarrierPathSegment::CallerKind),
            });
        }
    })
}

fn streaming_requirement_discriminant(
    streaming: InvocationStreamingRequirement,
) -> Result<u8, InvocationCarrierCodecError> {
    Ok(match streaming {
        InvocationStreamingRequirement::Unspecified => 0,
        InvocationStreamingRequirement::Required => 1,
        InvocationStreamingRequirement::Preferred => 2,
        InvocationStreamingRequirement::Forbidden => 3,
        _ => {
            return Err(InvocationCarrierCodecError::InvalidField {
                path: InvocationCarrierPath::one(
                    InvocationCarrierPathSegment::RequestOutputRequirement,
                )
                .with(InvocationCarrierPathSegment::OutputStreaming),
            });
        }
    })
}

fn trace_policy_discriminant(
    trace: InvocationTracePolicy,
) -> Result<u8, InvocationCarrierCodecError> {
    Ok(match trace {
        InvocationTracePolicy::Off => 0,
        InvocationTracePolicy::Basic => 1,
        InvocationTracePolicy::Normal => 2,
        InvocationTracePolicy::Verbose => 3,
        InvocationTracePolicy::Profile => 4,
        _ => {
            return Err(InvocationCarrierCodecError::InvalidField {
                path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestTracePolicy),
            });
        }
    })
}

fn event_kind_discriminant(body: &InvocationEventBody) -> Result<u8, InvocationCarrierCodecError> {
    Ok(match body {
        InvocationEventBody::Started { .. } => 0,
        InvocationEventBody::ValueBatch { .. } => 1,
        InvocationEventBody::Diagnostic(_) => 2,
        InvocationEventBody::Completed { .. } => 3,
        InvocationEventBody::Failed(_) => 4,
        InvocationEventBody::Cancelled { .. } => 5,
        _ => {
            return Err(InvocationCarrierCodecError::InvalidField {
                path: InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody),
            });
        }
    })
}

fn encode_invoke_event_payload(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    event: &InvokeEvent,
) -> Result<Vec<u8>, InvocationCarrierCodecError> {
    if event.node_count() > MAX_INVOCATION_CARRIER_NODES {
        return Err(InvocationCarrierCodecError::TooManyNodes {
            maximum: MAX_INVOCATION_CARRIER_NODES,
        });
    }
    let mut writer = CarrierWriter::new();
    writer.u8(1)?;
    writer.u8(event_kind_discriminant(event.body())?)?;
    writer.append(&event.invocation_id().to_bytes())?;
    writer.u64(event.sequence())?;
    match event.body() {
        InvocationEventBody::Started { visible_principal } => {
            append_optional_id(&mut writer, *visible_principal, PrincipalId::to_bytes)?;
        }
        InvocationEventBody::ValueBatch { schema, values } => {
            writer.u8(0)?;
            append_optional_invoke_value(
                &mut writer,
                active,
                registry,
                schema.as_ref(),
                InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody)
                    .with(InvocationCarrierPathSegment::Schema),
            )?;
            if values.is_empty() {
                return Err(InvocationCarrierCodecError::InvalidField {
                    path: InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody)
                        .with(InvocationCarrierPathSegment::BatchValues),
                });
            }
            writer.count(values.len())?;
            for (index, value) in values.iter().enumerate() {
                append_embedded_invoke_value(
                    &mut writer,
                    active,
                    registry,
                    value,
                    InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody)
                        .with(InvocationCarrierPathSegment::BatchValues)
                        .with(InvocationCarrierPathSegment::BatchValue(index)),
                )?;
            }
        }
        InvocationEventBody::Diagnostic(diagnostic) => {
            writer.u8(match diagnostic.severity() {
                InvocationDiagnosticSeverity::Info => 0,
                InvocationDiagnosticSeverity::Warning => 1,
                InvocationDiagnosticSeverity::Error => 2,
                _ => {
                    return Err(InvocationCarrierCodecError::InvalidField {
                        path: InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody)
                            .with(InvocationCarrierPathSegment::Severity),
                    });
                }
            })?;
            writer.text(diagnostic.code())?;
            writer.text(diagnostic.message())?;
        }
        InvocationEventBody::Completed {
            duration_nanoseconds,
        } => writer.u64(*duration_nanoseconds)?,
        InvocationEventBody::Failed(failure) => {
            writer.u8(failure_phase_discriminant(failure.phase())?)?;
            writer.text(failure.code())?;
            writer.text(failure.message())?;
            append_optional_invoke_value(
                &mut writer,
                active,
                registry,
                failure.details(),
                InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody)
                    .with(InvocationCarrierPathSegment::Details),
            )?;
            writer.u8(retryability_discriminant(failure.retryability())?)?;
        }
        InvocationEventBody::Cancelled { reason } => {
            append_optional_text(&mut writer, reason.as_deref())?;
        }
        _ => {
            return Err(InvocationCarrierCodecError::InvalidField {
                path: InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody),
            });
        }
    }
    Ok(writer.finish())
}

fn failure_phase_discriminant(
    phase: InvocationFailurePhase,
) -> Result<u8, InvocationCarrierCodecError> {
    Ok(match phase {
        InvocationFailurePhase::Resolve => 0,
        InvocationFailurePhase::Bind => 1,
        InvocationFailurePhase::Authorise => 2,
        InvocationFailurePhase::Target => 3,
        InvocationFailurePhase::Present => 4,
        InvocationFailurePhase::Runtime => 5,
        InvocationFailurePhase::Transport => 6,
        InvocationFailurePhase::Internal => 7,
        _ => {
            return Err(InvocationCarrierCodecError::InvalidField {
                path: InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody)
                    .with(InvocationCarrierPathSegment::Phase),
            });
        }
    })
}

fn retryability_discriminant(
    retryability: InvocationRetryability,
) -> Result<u8, InvocationCarrierCodecError> {
    Ok(match retryability {
        InvocationRetryability::Unknown => 0,
        InvocationRetryability::No => 1,
        InvocationRetryability::Yes => 2,
        _ => {
            return Err(InvocationCarrierCodecError::InvalidField {
                path: InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody)
                    .with(InvocationCarrierPathSegment::Retryability),
            });
        }
    })
}

struct CarrierReader<'a> {
    bytes: &'a [u8],
    cursor: usize,
    base: usize,
}

impl<'a> CarrierReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            cursor: 0,
            base: 0,
        }
    }

    fn with_base(bytes: &'a [u8], base: usize) -> Self {
        Self {
            bytes,
            cursor: 0,
            base,
        }
    }

    fn position(&self) -> usize {
        self.cursor
    }

    fn absolute_position(&self) -> usize {
        self.base.saturating_add(self.cursor)
    }

    fn slice(&self, start: usize, end: usize) -> &'a [u8] {
        &self.bytes[start..end]
    }

    fn take(&mut self, required: usize) -> Result<&'a [u8], InvocationCarrierCodecError> {
        let available = self.bytes.len().saturating_sub(self.cursor);
        if available < required {
            return Err(InvocationCarrierCodecError::Truncated {
                offset: self.absolute_position(),
                required,
                available,
            });
        }
        let end = self.cursor.checked_add(required).ok_or(
            InvocationCarrierCodecError::PayloadTooLarge {
                actual: usize::MAX,
                maximum: PAYLOAD_LIMIT,
            },
        )?;
        let value = &self.bytes[self.cursor..end];
        self.cursor = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, InvocationCarrierCodecError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, InvocationCarrierCodecError> {
        Ok(u16::from_be_bytes(
            self.take(2)?.try_into().expect("reader returned two bytes"),
        ))
    }

    fn u32(&mut self) -> Result<u32, InvocationCarrierCodecError> {
        Ok(u32::from_be_bytes(
            self.take(4)?
                .try_into()
                .expect("reader returned four bytes"),
        ))
    }

    fn i32(&mut self) -> Result<i32, InvocationCarrierCodecError> {
        Ok(i32::from_be_bytes(
            self.take(4)?
                .try_into()
                .expect("reader returned four bytes"),
        ))
    }

    fn u64(&mut self) -> Result<u64, InvocationCarrierCodecError> {
        Ok(u64::from_be_bytes(
            self.take(8)?
                .try_into()
                .expect("reader returned eight bytes"),
        ))
    }

    fn id(&mut self) -> Result<[u8; 16], InvocationCarrierCodecError> {
        Ok(self
            .take(16)?
            .try_into()
            .expect("reader returned sixteen bytes"))
    }

    fn length_prefixed(&mut self) -> Result<CarrierSpan<'a>, InvocationCarrierCodecError> {
        let length = self.u32()? as usize;
        let offset = self.absolute_position();
        let bytes = self.take(length)?;
        Ok(CarrierSpan { bytes, offset })
    }

    fn text(
        &mut self,
        path: InvocationCarrierPath,
    ) -> Result<&'a str, InvocationCarrierCodecError> {
        let span = self.length_prefixed()?;
        std::str::from_utf8(span.bytes)
            .map_err(|_| InvocationCarrierCodecError::InvalidText { path })
    }

    fn required_text(
        &mut self,
        path: InvocationCarrierPath,
    ) -> Result<&'a str, InvocationCarrierCodecError> {
        let value = self.text(path.clone())?;
        if value.is_empty() {
            Err(InvocationCarrierCodecError::InvalidField { path })
        } else {
            Ok(value)
        }
    }

    fn presence(
        &mut self,
        path: InvocationCarrierPath,
    ) -> Result<bool, InvocationCarrierCodecError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            actual => Err(InvocationCarrierCodecError::InvalidBoolean { path, actual }),
        }
    }

    fn boolean(
        &mut self,
        path: InvocationCarrierPath,
    ) -> Result<bool, InvocationCarrierCodecError> {
        self.presence(path)
    }

    fn semantic_name(
        &mut self,
        path: InvocationCarrierPath,
    ) -> Result<Vec<&'a str>, InvocationCarrierCodecError> {
        let count = self.u32()? as usize;
        if count < 2 {
            return Err(InvocationCarrierCodecError::InvalidSemanticName { path });
        }
        let mut parts = Vec::new();
        for _ in 0..count {
            let part = self.text(path.clone())?;
            if part.is_empty() {
                return Err(InvocationCarrierCodecError::InvalidSemanticName { path });
            }
            parts.push(part);
        }
        Ok(parts)
    }

    fn finish(self) -> Result<(), InvocationCarrierCodecError> {
        if self.cursor == self.bytes.len() {
            Ok(())
        } else {
            Err(InvocationCarrierCodecError::Trailing {
                remaining: self.bytes.len() - self.cursor,
            })
        }
    }
}

#[derive(Clone, Copy)]
struct CarrierSpan<'a> {
    bytes: &'a [u8],
    offset: usize,
}

struct ValidatedInvokeValueWire<'a> {
    inner: CarrierSpan<'a>,
    path: InvocationCarrierPath,
}

enum ValidatedInvocationTargetWire<'a> {
    FunctionId(FunctionId),
    QualifiedName(Vec<&'a str>),
}

enum ValidatedParameterSelectorWire<'a> {
    ParameterId(ParameterId),
    Name(&'a str),
}

struct ValidatedArgumentWire<'a> {
    selector: ValidatedParameterSelectorWire<'a>,
    value: ValidatedInvokeValueWire<'a>,
}

struct ValidatedCallerWire<'a> {
    kind: InvocationCallerKind,
    interactive: bool,
    stdout_is_tty: bool,
    terminal_columns: Option<u32>,
    terminal_rows: Option<u32>,
    locale: &'a str,
    timezone: &'a str,
    preference_policy: Option<ValidatedInvokeValueWire<'a>>,
}

struct ValidatedSinkWire<'a> {
    descriptor: TypeDescriptor,
    media_types: Vec<&'a str>,
    streaming: bool,
    preference_rank: i32,
    limits: Option<ValidatedInvokeValueWire<'a>>,
}

struct ValidatedContractWire<'a> {
    name: &'a str,
    version: &'a str,
    features: Vec<&'a str>,
}

struct ValidatedRuntimeWire<'a> {
    name: &'a str,
    version: &'a str,
    consumed_descriptors: Vec<TypeDescriptor>,
    contracts: Vec<ValidatedContractWire<'a>>,
    preference_rank: i32,
    trusted: bool,
    limits: Option<ValidatedInvokeValueWire<'a>>,
}

struct ValidatedClientOfferWire<'a> {
    protocol_major: u16,
    locale: &'a str,
    timezone: &'a str,
    sinks: Vec<ValidatedSinkWire<'a>>,
    runtimes: Vec<ValidatedRuntimeWire<'a>>,
    maximum_frame_size: u32,
    maximum_artifact_size: u64,
    limits: Option<ValidatedInvokeValueWire<'a>>,
    preferences: Option<ValidatedInvokeValueWire<'a>>,
}

enum ValidatedOutputTypeWire<'a> {
    TypeId(TypeId),
    QualifiedName(Vec<&'a str>),
}

struct ValidatedOutputWire<'a> {
    alias: Option<&'a str>,
    media_type: Option<&'a str>,
    type_selector: Option<ValidatedOutputTypeWire<'a>>,
    streaming: InvocationStreamingRequirement,
}

struct ValidatedRequestWire<'a> {
    target: ValidatedInvocationTargetWire<'a>,
    arguments: Vec<ValidatedArgumentWire<'a>>,
    caller: ValidatedCallerWire<'a>,
    client_offer: ValidatedClientOfferWire<'a>,
    output: Option<ValidatedOutputWire<'a>>,
    state_profile: Option<&'a str>,
    trace_policy: InvocationTracePolicy,
    idempotency_key: Option<&'a [u8]>,
    parent_invocation: Option<InvocationId>,
    observer_context: Option<ValidatedInvokeValueWire<'a>>,
}

enum ValidatedEventBodyWire<'a> {
    Started {
        visible_principal: Option<PrincipalId>,
    },
    ValueBatch {
        schema: Option<ValidatedInvokeValueWire<'a>>,
        values: Vec<ValidatedInvokeValueWire<'a>>,
    },
    Diagnostic {
        severity: InvocationDiagnosticSeverity,
        code: &'a str,
        message: &'a str,
    },
    Completed {
        duration_nanoseconds: u64,
    },
    Failed {
        phase: InvocationFailurePhase,
        code: &'a str,
        message: &'a str,
        details: Option<ValidatedInvokeValueWire<'a>>,
        retryability: InvocationRetryability,
    },
    Cancelled {
        reason: Option<&'a str>,
    },
}

struct ValidatedEventWire<'a> {
    invocation_id: InvocationId,
    sequence: u64,
    body: ValidatedEventBodyWire<'a>,
}

enum ValidatedCarrierWire<'a> {
    Value(ValidatedInvokeValueWire<'a>),
    Request(Box<ValidatedRequestWire<'a>>),
    Event(ValidatedEventWire<'a>),
}

fn parse_invocation_carrier<'a>(
    carrier: TypeId,
    payload: &'a [u8],
) -> Result<ValidatedCarrierWire<'a>, InvocationCarrierCodecError> {
    let mut reader = CarrierReader::new(payload);
    let version = reader.u8()?;
    if version != 1 {
        return Err(InvocationCarrierCodecError::UnsupportedVersion { actual: version });
    }
    let validated = if carrier == SYS_INVOKE_VALUE_TYPE_ID {
        ValidatedCarrierWire::Value(parse_invoke_value_wire(
            &mut reader,
            InvocationCarrierPath::one(InvocationCarrierPathSegment::ValueInner),
        )?)
    } else if carrier == SYS_INVOKE_REQUEST_TYPE_ID {
        ValidatedCarrierWire::Request(Box::new(parse_request_wire(&mut reader)?))
    } else if carrier == SYS_INVOKE_EVENT_TYPE_ID {
        ValidatedCarrierWire::Event(parse_event_wire(&mut reader)?)
    } else {
        unreachable!("only registry carrier identities reach the carrier parser")
    };
    reader.finish()?;
    Ok(validated)
}

fn parse_invoke_value_wire<'a>(
    reader: &mut CarrierReader<'a>,
    path: InvocationCarrierPath,
) -> Result<ValidatedInvokeValueWire<'a>, InvocationCarrierCodecError> {
    Ok(ValidatedInvokeValueWire {
        inner: reader.length_prefixed()?,
        path,
    })
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SelectorKey(Vec<u8>);

fn require_increasing<K: Ord>(
    previous: &mut Option<(K, usize)>,
    key: K,
    index: usize,
    path: &InvocationCarrierPath,
) -> Result<(), InvocationCarrierCodecError> {
    if let Some((previous_key, previous_index)) = previous {
        match (*previous_key).cmp(&key) {
            Ordering::Less => {}
            Ordering::Equal => {
                return Err(InvocationCarrierCodecError::DuplicateItem {
                    path: path.clone(),
                    first: *previous_index,
                    duplicate: index,
                });
            }
            Ordering::Greater => {
                return Err(InvocationCarrierCodecError::NonCanonicalOrder {
                    path: path.clone(),
                    index,
                });
            }
        }
    }
    *previous = Some((key, index));
    Ok(())
}

fn parse_request_wire<'a>(
    reader: &mut CarrierReader<'a>,
) -> Result<ValidatedRequestWire<'a>, InvocationCarrierCodecError> {
    let target_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestTarget);
    let target = match reader.u8()? {
        0 => ValidatedInvocationTargetWire::FunctionId(FunctionId::from_bytes(reader.id()?)),
        1 => {
            ValidatedInvocationTargetWire::QualifiedName(reader.semantic_name(target_path.clone())?)
        }
        actual => {
            return Err(InvocationCarrierCodecError::UnknownDiscriminant {
                path: target_path,
                actual,
            });
        }
    };

    let arguments_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestArguments);
    let argument_count = reader.u32()? as usize;
    let mut arguments = Vec::new();
    let mut previous_selector = None;
    for index in 0..argument_count {
        let argument_path = arguments_path.with(InvocationCarrierPathSegment::Argument(index));
        let selector_path = argument_path.with(InvocationCarrierPathSegment::Selector);
        let (selector, key) = match reader.u8()? {
            0 => {
                let bytes = reader.id()?;
                let mut key = vec![0];
                key.extend_from_slice(&bytes);
                (
                    ValidatedParameterSelectorWire::ParameterId(ParameterId::from_bytes(bytes)),
                    SelectorKey(key),
                )
            }
            1 => {
                let name = reader.required_text(selector_path.clone())?;
                let mut key = vec![1];
                key.extend_from_slice(name.as_bytes());
                (ValidatedParameterSelectorWire::Name(name), SelectorKey(key))
            }
            actual => {
                return Err(InvocationCarrierCodecError::UnknownDiscriminant {
                    path: selector_path,
                    actual,
                });
            }
        };
        require_increasing(&mut previous_selector, key, index, &arguments_path)?;
        let value = parse_embedded_invoke_value(
            reader,
            argument_path.with(InvocationCarrierPathSegment::Value),
        )?;
        arguments.push(ValidatedArgumentWire { selector, value });
    }

    let caller = parse_caller_wire(reader)?;
    let client_offer = parse_client_offer_wire(reader)?;
    let output = parse_output_wire(reader)?;
    let state_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestStateProfile);
    let state_profile = if reader.presence(state_path.clone())? {
        Some(reader.required_text(state_path)?)
    } else {
        None
    };
    let trace_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestTracePolicy);
    let trace_policy = match reader.u8()? {
        0 => InvocationTracePolicy::Off,
        1 => InvocationTracePolicy::Basic,
        2 => InvocationTracePolicy::Normal,
        3 => InvocationTracePolicy::Verbose,
        4 => InvocationTracePolicy::Profile,
        actual => {
            return Err(InvocationCarrierCodecError::UnknownDiscriminant {
                path: trace_path,
                actual,
            });
        }
    };
    let deadline_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestDeadline);
    match reader.u8()? {
        0 => {}
        1 => {
            return Err(InvocationCarrierCodecError::InvalidField {
                path: deadline_path,
            });
        }
        actual => {
            return Err(InvocationCarrierCodecError::InvalidBoolean {
                path: deadline_path,
                actual,
            });
        }
    }
    let idempotency_path =
        InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestIdempotencyKey);
    let idempotency_key = if reader.presence(idempotency_path.clone())? {
        let span = reader.length_prefixed()?;
        if span.bytes.is_empty() {
            return Err(InvocationCarrierCodecError::InvalidField {
                path: idempotency_path,
            });
        }
        Some(span.bytes)
    } else {
        None
    };
    let parent_path =
        InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestParentInvocation);
    let parent_invocation = if reader.presence(parent_path)? {
        Some(InvocationId::from_bytes(reader.id()?))
    } else {
        None
    };
    let observer_path =
        InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestObserverContext);
    let observer_context = if reader.presence(observer_path.clone())? {
        Some(parse_embedded_invoke_value(reader, observer_path)?)
    } else {
        None
    };
    Ok(ValidatedRequestWire {
        target,
        arguments,
        caller,
        client_offer,
        output,
        state_profile,
        trace_policy,
        idempotency_key,
        parent_invocation,
        observer_context,
    })
}

fn parse_optional_u32(
    reader: &mut CarrierReader<'_>,
    path: InvocationCarrierPath,
) -> Result<Option<u32>, InvocationCarrierCodecError> {
    if reader.presence(path.clone())? {
        let value = reader.u32()?;
        if value == 0 {
            return Err(InvocationCarrierCodecError::InvalidField { path });
        }
        Ok(Some(value))
    } else {
        Ok(None)
    }
}

fn parse_caller_wire<'a>(
    reader: &mut CarrierReader<'a>,
) -> Result<ValidatedCallerWire<'a>, InvocationCarrierCodecError> {
    let caller_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestCaller);
    let kind_path = caller_path.with(InvocationCarrierPathSegment::CallerKind);
    let kind = match reader.u8()? {
        0 => InvocationCallerKind::CliTty,
        1 => InvocationCallerKind::CliPipe,
        2 => InvocationCallerKind::DesktopLauncher,
        3 => InvocationCallerKind::Browser,
        4 => InvocationCallerKind::ClientFunction,
        5 => InvocationCallerKind::JsonRpcGateway,
        6 => InvocationCallerKind::McpGateway,
        7 => InvocationCallerKind::Scheduler,
        8 => InvocationCallerKind::TestRunner,
        9 => InvocationCallerKind::Recovery,
        actual => {
            return Err(InvocationCarrierCodecError::UnknownDiscriminant {
                path: kind_path,
                actual,
            });
        }
    };
    let flags_path = caller_path.with(InvocationCarrierPathSegment::CallerFlags);
    let flags = reader.u8()?;
    if flags & !0b11 != 0 {
        return Err(InvocationCarrierCodecError::InvalidField { path: flags_path });
    }
    let interactive = flags & 1 != 0;
    let stdout_is_tty = flags & 2 != 0;
    let terminal_columns = parse_optional_u32(
        reader,
        caller_path.with(InvocationCarrierPathSegment::TerminalColumns),
    )?;
    let terminal_rows = parse_optional_u32(
        reader,
        caller_path.with(InvocationCarrierPathSegment::TerminalRows),
    )?;
    if (kind == InvocationCallerKind::CliTty
        && (!interactive
            || !stdout_is_tty
            || terminal_columns.is_none()
            || terminal_rows.is_none()))
        || (kind == InvocationCallerKind::CliPipe && (interactive || stdout_is_tty))
    {
        return Err(InvocationCarrierCodecError::InvalidField { path: caller_path });
    }
    let locale = reader.required_text(caller_path.with(InvocationCarrierPathSegment::Locale))?;
    let timezone =
        reader.required_text(caller_path.with(InvocationCarrierPathSegment::Timezone))?;
    let policy_path = caller_path.with(InvocationCarrierPathSegment::PreferencePolicy);
    let preference_policy = if reader.presence(policy_path.clone())? {
        Some(parse_embedded_invoke_value(reader, policy_path)?)
    } else {
        None
    };
    Ok(ValidatedCallerWire {
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

fn parse_embedded_invoke_value<'a>(
    reader: &mut CarrierReader<'a>,
    path: InvocationCarrierPath,
) -> Result<ValidatedInvokeValueWire<'a>, InvocationCarrierCodecError> {
    let envelope = reader.length_prefixed()?;
    let (tag, type_id, payload) = decode_envelope(envelope.bytes, CONSTRUCTED_MARKER)
        .map_err(|source| map_embedded_envelope_error(source, envelope, path.clone()))?;
    if type_id != SYS_INVOKE_VALUE_TYPE_ID || tag != OPAQUE_TAG {
        if invocation_carrier_by_id(type_id).is_some() {
            return Err(InvocationCarrierCodecError::NestedCarrier {
                path,
                carrier: type_id,
            });
        }
        return Err(InvocationCarrierCodecError::InvalidField { path });
    }
    let payload_offset = envelope.offset.saturating_add(HEADER_LENGTH);
    let mut nested = CarrierReader::with_base(payload, payload_offset);
    let version = nested.u8()?;
    if version != 1 {
        return Err(InvocationCarrierCodecError::UnsupportedVersion { actual: version });
    }
    let value = parse_invoke_value_wire(&mut nested, path)?;
    nested.finish()?;
    Ok(value)
}

fn map_embedded_envelope_error(
    source: ValueCodecError,
    span: CarrierSpan<'_>,
    path: InvocationCarrierPath,
) -> InvocationCarrierCodecError {
    match source {
        ValueCodecError::TruncatedHeader { actual } => InvocationCarrierCodecError::Truncated {
            offset: span.offset,
            required: HEADER_LENGTH,
            available: actual,
        },
        ValueCodecError::TruncatedPayload { declared, actual } => {
            InvocationCarrierCodecError::Truncated {
                offset: span.offset.saturating_add(HEADER_LENGTH),
                required: declared,
                available: actual,
            }
        }
        ValueCodecError::TrailingBytes { declared, actual } => {
            InvocationCarrierCodecError::Trailing {
                remaining: actual - declared,
            }
        }
        ValueCodecError::PayloadTooLarge { actual, maximum } => {
            InvocationCarrierCodecError::PayloadTooLarge { actual, maximum }
        }
        _ => InvocationCarrierCodecError::InvalidField { path },
    }
}

fn parse_descriptor(
    reader: &mut CarrierReader<'_>,
    path: InvocationCarrierPath,
) -> Result<(TypeDescriptor, DescriptorKey), InvocationCarrierCodecError> {
    let length = reader.u16()? as usize;
    if length == 0 {
        return Err(InvocationCarrierCodecError::InvalidField { path });
    }
    let offset = reader.absolute_position();
    let bytes = reader.take(length)?;
    let (descriptor, consumed) =
        parse_constructed_descriptor(bytes, 0, true).map_err(|source| match source {
            ValueCodecError::TruncatedConstructedDescriptorNode {
                offset: inner,
                required,
                available,
            } => InvocationCarrierCodecError::Truncated {
                offset: offset.saturating_add(inner),
                required,
                available,
            },
            ValueCodecError::UnknownConstructedDescriptorTag { tag } => {
                InvocationCarrierCodecError::UnknownDiscriminant {
                    path: path.clone(),
                    actual: tag,
                }
            }
            _ => InvocationCarrierCodecError::InvalidField { path: path.clone() },
        })?;
    if consumed != bytes.len() {
        return Err(InvocationCarrierCodecError::InvalidField { path });
    }
    reject_carrier_descriptor(&descriptor, &path)?;
    Ok((descriptor, DescriptorKey(bytes.to_vec())))
}

enum CanonicalTextItem {
    MediaType,
    Feature,
}

fn parse_canonical_text_list<'a>(
    reader: &mut CarrierReader<'a>,
    path: &InvocationCarrierPath,
    item: CanonicalTextItem,
) -> Result<(Vec<&'a str>, Vec<u8>), InvocationCarrierCodecError> {
    let start = reader.position();
    let count = reader.u32()? as usize;
    let mut values = Vec::new();
    let mut previous = None;
    for index in 0..count {
        let item_path = path.with(match item {
            CanonicalTextItem::MediaType => InvocationCarrierPathSegment::MediaType(index),
            CanonicalTextItem::Feature => InvocationCarrierPathSegment::Feature(index),
        });
        let value = reader.required_text(item_path)?;
        require_increasing(
            &mut previous,
            TextKey(value.as_bytes().to_vec()),
            index,
            path,
        )?;
        values.push(value);
    }
    let encoded = reader.slice(start, reader.position()).to_vec();
    Ok((values, encoded))
}

fn parse_descriptor_list(
    reader: &mut CarrierReader<'_>,
    path: &InvocationCarrierPath,
) -> Result<Vec<TypeDescriptor>, InvocationCarrierCodecError> {
    let count = reader.u32()? as usize;
    let mut values = Vec::new();
    let mut previous = None;
    for index in 0..count {
        let item_path = path.with(InvocationCarrierPathSegment::ConsumedType(index));
        let (descriptor, key) = parse_descriptor(reader, item_path)?;
        require_increasing(&mut previous, key, index, path)?;
        values.push(descriptor);
    }
    Ok(values)
}

fn parse_contract_list<'a>(
    reader: &mut CarrierReader<'a>,
    path: &InvocationCarrierPath,
) -> Result<Vec<ValidatedContractWire<'a>>, InvocationCarrierCodecError> {
    let count = reader.u32()? as usize;
    let mut values = Vec::new();
    let mut previous = None;
    for index in 0..count {
        let item_path = path.with(InvocationCarrierPathSegment::Contract(index));
        let name =
            reader.required_text(item_path.with(InvocationCarrierPathSegment::ContractName))?;
        let version =
            reader.required_text(item_path.with(InvocationCarrierPathSegment::ContractVersion))?;
        let features_path = item_path.with(InvocationCarrierPathSegment::Features);
        let (features, feature_bytes) =
            parse_canonical_text_list(reader, &features_path, CanonicalTextItem::Feature)?;
        let key = ContractKey {
            name: TextKey(name.as_bytes().to_vec()),
            version: TextKey(version.as_bytes().to_vec()),
            features: feature_bytes,
        };
        require_increasing(&mut previous, key, index, path)?;
        values.push(ValidatedContractWire {
            name,
            version,
            features,
        });
    }
    Ok(values)
}

fn parse_sink_list<'a>(
    reader: &mut CarrierReader<'a>,
    path: &InvocationCarrierPath,
) -> Result<Vec<ValidatedSinkWire<'a>>, InvocationCarrierCodecError> {
    let count = reader.u32()? as usize;
    let mut values = Vec::new();
    let mut previous = None;
    for index in 0..count {
        let item_path = path.with(InvocationCarrierPathSegment::Sink(index));
        let (descriptor, descriptor_key) = parse_descriptor(
            reader,
            item_path.with(InvocationCarrierPathSegment::Descriptor),
        )?;
        let media_path = item_path.with(InvocationCarrierPathSegment::MediaTypes);
        let (media_types, media_bytes) =
            parse_canonical_text_list(reader, &media_path, CanonicalTextItem::MediaType)?;
        let streaming = reader.boolean(item_path.with(InvocationCarrierPathSegment::Streaming))?;
        let preference_rank = reader.i32()?;
        let limits_path = item_path.with(InvocationCarrierPathSegment::Limits);
        let limits = if reader.presence(limits_path.clone())? {
            Some(parse_embedded_invoke_value(reader, limits_path)?)
        } else {
            None
        };
        let key = SinkKey {
            descriptor: descriptor_key,
            media_types: media_bytes,
            streaming: u8::from(streaming),
            preference_rank: preference_rank.to_be_bytes(),
        };
        require_increasing(&mut previous, key, index, path)?;
        values.push(ValidatedSinkWire {
            descriptor,
            media_types,
            streaming,
            preference_rank,
            limits,
        });
    }
    Ok(values)
}

fn parse_runtime_list<'a>(
    reader: &mut CarrierReader<'a>,
    path: &InvocationCarrierPath,
) -> Result<Vec<ValidatedRuntimeWire<'a>>, InvocationCarrierCodecError> {
    let count = reader.u32()? as usize;
    let mut values = Vec::new();
    let mut previous = None;
    for index in 0..count {
        let item_path = path.with(InvocationCarrierPathSegment::Runtime(index));
        let name =
            reader.required_text(item_path.with(InvocationCarrierPathSegment::RuntimeName))?;
        let version =
            reader.required_text(item_path.with(InvocationCarrierPathSegment::RuntimeVersion))?;
        let remaining_start = reader.position();
        let consumed_path = item_path.with(InvocationCarrierPathSegment::ConsumedTypes);
        let consumed_descriptors = parse_descriptor_list(reader, &consumed_path)?;
        let contracts_path = item_path.with(InvocationCarrierPathSegment::Contracts);
        let contracts = parse_contract_list(reader, &contracts_path)?;
        let preference_rank = reader.i32()?;
        let trusted = reader.boolean(item_path.with(InvocationCarrierPathSegment::Trusted))?;
        let limits_path = item_path.with(InvocationCarrierPathSegment::Limits);
        let limits = if reader.presence(limits_path.clone())? {
            Some(parse_embedded_invoke_value(reader, limits_path)?)
        } else {
            None
        };
        let remaining = reader.slice(remaining_start, reader.position()).to_vec();
        let key = RuntimeKey {
            name: TextKey(name.as_bytes().to_vec()),
            version: TextKey(version.as_bytes().to_vec()),
            remaining,
        };
        require_increasing(&mut previous, key, index, path)?;
        values.push(ValidatedRuntimeWire {
            name,
            version,
            consumed_descriptors,
            contracts,
            preference_rank,
            trusted,
            limits,
        });
    }
    Ok(values)
}

fn parse_client_offer_wire<'a>(
    reader: &mut CarrierReader<'a>,
) -> Result<ValidatedClientOfferWire<'a>, InvocationCarrierCodecError> {
    let offer_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestClientOffer);
    let protocol_major = reader.u16()?;
    if protocol_major != 5 {
        return Err(InvocationCarrierCodecError::InvalidField {
            path: offer_path.with(InvocationCarrierPathSegment::ClientProtocol),
        });
    }
    let locale =
        reader.required_text(offer_path.with(InvocationCarrierPathSegment::ClientLocale))?;
    let timezone =
        reader.required_text(offer_path.with(InvocationCarrierPathSegment::ClientTimezone))?;
    let sinks_path = offer_path.with(InvocationCarrierPathSegment::ClientSinks);
    let sinks = parse_sink_list(reader, &sinks_path)?;
    let runtimes_path = offer_path.with(InvocationCarrierPathSegment::ClientRuntimes);
    let runtimes = parse_runtime_list(reader, &runtimes_path)?;
    let maximum_frame_size = reader.u32()?;
    if maximum_frame_size < 1_024 {
        return Err(InvocationCarrierCodecError::InvalidField {
            path: offer_path.with(InvocationCarrierPathSegment::ClientMaximumFrameSize),
        });
    }
    let maximum_artifact_size = reader.u64()?;
    let limits_path = offer_path.with(InvocationCarrierPathSegment::ClientLimits);
    let limits = if reader.presence(limits_path.clone())? {
        Some(parse_embedded_invoke_value(reader, limits_path)?)
    } else {
        None
    };
    let preferences_path = offer_path.with(InvocationCarrierPathSegment::ClientPreferences);
    let preferences = if reader.presence(preferences_path.clone())? {
        Some(parse_embedded_invoke_value(reader, preferences_path)?)
    } else {
        None
    };
    Ok(ValidatedClientOfferWire {
        protocol_major,
        locale,
        timezone,
        sinks,
        runtimes,
        maximum_frame_size,
        maximum_artifact_size,
        limits,
        preferences,
    })
}

fn parse_optional_text<'a>(
    reader: &mut CarrierReader<'a>,
    path: InvocationCarrierPath,
) -> Result<Option<&'a str>, InvocationCarrierCodecError> {
    if reader.presence(path.clone())? {
        Ok(Some(reader.required_text(path)?))
    } else {
        Ok(None)
    }
}

fn parse_output_wire<'a>(
    reader: &mut CarrierReader<'a>,
) -> Result<Option<ValidatedOutputWire<'a>>, InvocationCarrierCodecError> {
    let output_path =
        InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestOutputRequirement);
    if !reader.presence(output_path.clone())? {
        return Ok(None);
    }
    let alias = parse_optional_text(
        reader,
        output_path.with(InvocationCarrierPathSegment::OutputAlias),
    )?;
    let media_type = parse_optional_text(
        reader,
        output_path.with(InvocationCarrierPathSegment::OutputMediaType),
    )?;
    let type_path = output_path.with(InvocationCarrierPathSegment::OutputType);
    let type_selector = if reader.presence(type_path.clone())? {
        Some(match reader.u8()? {
            0 => ValidatedOutputTypeWire::TypeId(TypeId::from_bytes(reader.id()?)),
            1 => ValidatedOutputTypeWire::QualifiedName(reader.semantic_name(type_path.clone())?),
            actual => {
                return Err(InvocationCarrierCodecError::UnknownDiscriminant {
                    path: type_path,
                    actual,
                });
            }
        })
    } else {
        None
    };
    if alias.is_none() && media_type.is_none() && type_selector.is_none() {
        return Err(InvocationCarrierCodecError::InvalidField { path: output_path });
    }
    let streaming_path = output_path.with(InvocationCarrierPathSegment::OutputStreaming);
    let streaming = match reader.u8()? {
        0 => InvocationStreamingRequirement::Unspecified,
        1 => InvocationStreamingRequirement::Required,
        2 => InvocationStreamingRequirement::Preferred,
        3 => InvocationStreamingRequirement::Forbidden,
        actual => {
            return Err(InvocationCarrierCodecError::UnknownDiscriminant {
                path: streaming_path,
                actual,
            });
        }
    };
    Ok(Some(ValidatedOutputWire {
        alias,
        media_type,
        type_selector,
        streaming,
    }))
}

fn parse_event_wire<'a>(
    reader: &mut CarrierReader<'a>,
) -> Result<ValidatedEventWire<'a>, InvocationCarrierCodecError> {
    let kind_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::EventKind);
    let kind = reader.u8()?;
    if kind > 5 {
        return Err(InvocationCarrierCodecError::UnknownDiscriminant {
            path: kind_path,
            actual: kind,
        });
    }
    let invocation_id = InvocationId::from_bytes(reader.id()?);
    let sequence = reader.u64()?;
    let body_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody);
    let body = match kind {
        0 => {
            let principal_path = body_path.with(InvocationCarrierPathSegment::VisiblePrincipal);
            let visible_principal = if reader.presence(principal_path)? {
                Some(PrincipalId::from_bytes(reader.id()?))
            } else {
                None
            };
            ValidatedEventBodyWire::Started { visible_principal }
        }
        1 => {
            let channel_path = body_path.with(InvocationCarrierPathSegment::Channel);
            let channel = reader.u8()?;
            if channel != 0 {
                return Err(InvocationCarrierCodecError::UnknownDiscriminant {
                    path: channel_path,
                    actual: channel,
                });
            }
            let schema_path = body_path.with(InvocationCarrierPathSegment::Schema);
            let schema = if reader.presence(schema_path.clone())? {
                Some(parse_embedded_invoke_value(reader, schema_path)?)
            } else {
                None
            };
            let values_path = body_path.with(InvocationCarrierPathSegment::BatchValues);
            let count = reader.u32()? as usize;
            if count == 0 {
                return Err(InvocationCarrierCodecError::InvalidField { path: values_path });
            }
            let mut values = Vec::new();
            for index in 0..count {
                values.push(parse_embedded_invoke_value(
                    reader,
                    values_path.with(InvocationCarrierPathSegment::BatchValue(index)),
                )?);
            }
            ValidatedEventBodyWire::ValueBatch { schema, values }
        }
        2 => {
            let severity_path = body_path.with(InvocationCarrierPathSegment::Severity);
            let severity = match reader.u8()? {
                0 => InvocationDiagnosticSeverity::Info,
                1 => InvocationDiagnosticSeverity::Warning,
                2 => InvocationDiagnosticSeverity::Error,
                actual => {
                    return Err(InvocationCarrierCodecError::UnknownDiscriminant {
                        path: severity_path,
                        actual,
                    });
                }
            };
            let code_path = body_path.with(InvocationCarrierPathSegment::Code);
            let code = reader.required_text(code_path.clone())?;
            if !is_printable_ascii(code) {
                return Err(InvocationCarrierCodecError::InvalidField { path: code_path });
            }
            let message = reader.text(body_path.with(InvocationCarrierPathSegment::Message))?;
            ValidatedEventBodyWire::Diagnostic {
                severity,
                code,
                message,
            }
        }
        3 => ValidatedEventBodyWire::Completed {
            duration_nanoseconds: reader.u64()?,
        },
        4 => {
            let phase_path = body_path.with(InvocationCarrierPathSegment::Phase);
            let phase = match reader.u8()? {
                0 => InvocationFailurePhase::Resolve,
                1 => InvocationFailurePhase::Bind,
                2 => InvocationFailurePhase::Authorise,
                3 => InvocationFailurePhase::Target,
                4 => InvocationFailurePhase::Present,
                5 => InvocationFailurePhase::Runtime,
                6 => InvocationFailurePhase::Transport,
                7 => InvocationFailurePhase::Internal,
                actual => {
                    return Err(InvocationCarrierCodecError::UnknownDiscriminant {
                        path: phase_path,
                        actual,
                    });
                }
            };
            let code_path = body_path.with(InvocationCarrierPathSegment::Code);
            let code = reader.required_text(code_path.clone())?;
            if !is_printable_ascii(code) {
                return Err(InvocationCarrierCodecError::InvalidField { path: code_path });
            }
            let message = reader.text(body_path.with(InvocationCarrierPathSegment::Message))?;
            let details_path = body_path.with(InvocationCarrierPathSegment::Details);
            let details = if reader.presence(details_path.clone())? {
                Some(parse_embedded_invoke_value(reader, details_path)?)
            } else {
                None
            };
            let retryability_path = body_path.with(InvocationCarrierPathSegment::Retryability);
            let retryability = match reader.u8()? {
                0 => InvocationRetryability::Unknown,
                1 => InvocationRetryability::No,
                2 => InvocationRetryability::Yes,
                actual => {
                    return Err(InvocationCarrierCodecError::UnknownDiscriminant {
                        path: retryability_path,
                        actual,
                    });
                }
            };
            ValidatedEventBodyWire::Failed {
                phase,
                code,
                message,
                details,
                retryability,
            }
        }
        5 => {
            let reason_path = body_path.with(InvocationCarrierPathSegment::Reason);
            ValidatedEventBodyWire::Cancelled {
                reason: parse_optional_text(reader, reason_path)?,
            }
        }
        _ => unreachable!("event kind range checked"),
    };
    Ok(ValidatedEventWire {
        invocation_id,
        sequence,
        body,
    })
}

fn is_printable_ascii(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
}

pub(super) fn decode_invocation_carrier(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    carrier: TypeId,
    payload: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    let validated = parse_invocation_carrier(carrier, payload)
        .map_err(|source| ValueCodecError::InvocationCarrier { carrier, source })?;
    validated
        .preflight(carrier)
        .map_err(|source| ValueCodecError::InvocationCarrier { carrier, source })?;
    validated
        .materialise(active, registry)
        .map_err(|source| ValueCodecError::InvocationCarrier { carrier, source })
}

impl ValidatedCarrierWire<'_> {
    fn preflight(&self, carrier: TypeId) -> Result<(), InvocationCarrierCodecError> {
        let mut budget = NodeBudget::invocation(carrier);
        match self {
            Self::Value(value) => preflight_validated_value(value, &mut budget, carrier, false),
            Self::Request(request) => {
                for argument in &request.arguments {
                    preflight_validated_value(&argument.value, &mut budget, carrier, true)?;
                }
                if let Some(value) = &request.caller.preference_policy {
                    preflight_validated_value(value, &mut budget, carrier, true)?;
                }
                for sink in &request.client_offer.sinks {
                    if let Some(value) = &sink.limits {
                        preflight_validated_value(value, &mut budget, carrier, true)?;
                    }
                }
                for runtime in &request.client_offer.runtimes {
                    if let Some(value) = &runtime.limits {
                        preflight_validated_value(value, &mut budget, carrier, true)?;
                    }
                }
                if let Some(value) = &request.client_offer.limits {
                    preflight_validated_value(value, &mut budget, carrier, true)?;
                }
                if let Some(value) = &request.client_offer.preferences {
                    preflight_validated_value(value, &mut budget, carrier, true)?;
                }
                if let Some(value) = &request.observer_context {
                    preflight_validated_value(value, &mut budget, carrier, true)?;
                }
                Ok(())
            }
            Self::Event(event) => {
                match &event.body {
                    ValidatedEventBodyWire::ValueBatch { schema, values } => {
                        if let Some(value) = schema {
                            preflight_validated_value(value, &mut budget, carrier, true)?;
                        }
                        for value in values {
                            preflight_validated_value(value, &mut budget, carrier, true)?;
                        }
                    }
                    ValidatedEventBodyWire::Failed {
                        details: Some(value),
                        ..
                    } => preflight_validated_value(value, &mut budget, carrier, true)?,
                    _ => {}
                }
                Ok(())
            }
        }
    }

    fn materialise(
        self,
        active: &ActiveDatabaseRevision,
        registry: &OpaqueCodecRegistry,
    ) -> Result<RuntimeValue, InvocationCarrierCodecError> {
        match self {
            Self::Value(value) => {
                materialise_invoke_value(active, registry, value).map(RuntimeValue::InvokeValue)
            }
            Self::Request(request) => {
                materialise_request(active, registry, *request).map(RuntimeValue::InvokeRequest)
            }
            Self::Event(event) => {
                materialise_event(active, registry, event).map(RuntimeValue::InvokeEvent)
            }
        }
    }
}

fn preflight_validated_value(
    value: &ValidatedInvokeValueWire<'_>,
    budget: &mut NodeBudget,
    outer: TypeId,
    add_wrapper: bool,
) -> Result<(), InvocationCarrierCodecError> {
    if add_wrapper {
        budget
            .increment()
            .map_err(extract_carrier_preflight_error)?;
    }
    preflight_orv5_envelope(
        value.inner.bytes,
        budget,
        &mut Vec::new(),
        InvocationCarrierPreflightPolicy::Reject {
            outer,
            path: &value.path,
        },
    )
    .map_err(|source| match source {
        ValueCodecError::InvocationCarrier {
            carrier: rejected_outer,
            source,
        } if rejected_outer == outer => source,
        source => InvocationCarrierCodecError::InnerValue {
            path: value.path.clone(),
            source: Box::new(source),
        },
    })
}

fn extract_carrier_preflight_error(source: ValueCodecError) -> InvocationCarrierCodecError {
    match source {
        ValueCodecError::InvocationCarrier { source, .. } => source,
        _ => InvocationCarrierCodecError::TooManyNodes {
            maximum: MAX_INVOCATION_CARRIER_NODES,
        },
    }
}

fn materialise_invoke_value(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: ValidatedInvokeValueWire<'_>,
) -> Result<InvokeValue, InvocationCarrierCodecError> {
    let decoded =
        decode_constructed_value(active, registry, value.inner.bytes).map_err(|source| {
            InvocationCarrierCodecError::InnerValue {
                path: value.path.clone(),
                source: Box::new(source),
            }
        })?;
    InvokeValue::new(decoded).map_err(|source| map_construction_error(source, value.path))
}

fn map_construction_error(
    source: InvocationCarrierConstructionError,
    path: InvocationCarrierPath,
) -> InvocationCarrierCodecError {
    match source {
        InvocationCarrierConstructionError::TooManyNodes { maximum } => {
            InvocationCarrierCodecError::TooManyNodes { maximum }
        }
        InvocationCarrierConstructionError::NestedCarrier { carrier } => {
            InvocationCarrierCodecError::NestedCarrier { path, carrier }
        }
        _ => InvocationCarrierCodecError::InvalidField { path },
    }
}

fn qualified_name(
    parts: Vec<&str>,
    path: InvocationCarrierPath,
) -> Result<QualifiedSemanticName, InvocationCarrierCodecError> {
    QualifiedSemanticName::new(parts)
        .map_err(|_| InvocationCarrierCodecError::InvalidSemanticName { path })
}

fn materialise_optional_value(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: Option<ValidatedInvokeValueWire<'_>>,
) -> Result<Option<InvokeValue>, InvocationCarrierCodecError> {
    value
        .map(|value| materialise_invoke_value(active, registry, value))
        .transpose()
}

fn materialise_request(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    request: ValidatedRequestWire<'_>,
) -> Result<InvokeRequest, InvocationCarrierCodecError> {
    let target_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestTarget);
    let target = match request.target {
        ValidatedInvocationTargetWire::FunctionId(function) => {
            InvocationTarget::function_id(function)
        }
        ValidatedInvocationTargetWire::QualifiedName(parts) => {
            InvocationTarget::qualified_name(qualified_name(parts, target_path.clone())?)
                .map_err(|source| map_construction_error(source, target_path.clone()))?
        }
    };
    let mut arguments = Vec::new();
    for (index, argument) in request.arguments.into_iter().enumerate() {
        let argument_path =
            InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestArguments)
                .with(InvocationCarrierPathSegment::Argument(index));
        let selector = match argument.selector {
            ValidatedParameterSelectorWire::ParameterId(parameter) => {
                InvocationParameterSelector::parameter_id(parameter)
            }
            ValidatedParameterSelectorWire::Name(name) => InvocationParameterSelector::name(name)
                .map_err(|source| {
                map_construction_error(
                    source,
                    argument_path.with(InvocationCarrierPathSegment::Selector),
                )
            })?,
        };
        let value = materialise_invoke_value(active, registry, argument.value)?;
        arguments.push(InvocationArgument::new(selector, value));
    }
    let caller = InvocationCallerContext::new(
        request.caller.kind,
        request.caller.interactive,
        request.caller.stdout_is_tty,
        request.caller.terminal_columns,
        request.caller.terminal_rows,
        request.caller.locale,
        request.caller.timezone,
        materialise_optional_value(active, registry, request.caller.preference_policy)?,
    )
    .map_err(|source| {
        map_construction_error(
            source,
            InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestCaller),
        )
    })?;
    let offer_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestClientOffer);
    let mut sinks = Vec::new();
    for (index, sink) in request.client_offer.sinks.into_iter().enumerate() {
        let item_path = offer_path
            .with(InvocationCarrierPathSegment::ClientSinks)
            .with(InvocationCarrierPathSegment::Sink(index));
        sinks.push(
            InvocationSinkOffer::new(
                sink.descriptor,
                sink.media_types,
                sink.streaming,
                sink.preference_rank,
                materialise_optional_value(active, registry, sink.limits)?,
            )
            .map_err(|source| map_construction_error(source, item_path))?,
        );
    }
    let mut runtimes = Vec::new();
    for (index, runtime) in request.client_offer.runtimes.into_iter().enumerate() {
        let item_path = offer_path
            .with(InvocationCarrierPathSegment::ClientRuntimes)
            .with(InvocationCarrierPathSegment::Runtime(index));
        let mut contracts = Vec::new();
        for (contract_index, contract) in runtime.contracts.into_iter().enumerate() {
            contracts.push(
                InvocationRuntimeContract::new(contract.name, contract.version, contract.features)
                    .map_err(|source| {
                        map_construction_error(
                            source,
                            item_path
                                .with(InvocationCarrierPathSegment::Contracts)
                                .with(InvocationCarrierPathSegment::Contract(contract_index)),
                        )
                    })?,
            );
        }
        runtimes.push(
            InvocationRuntimeOffer::new(
                runtime.name,
                runtime.version,
                runtime.consumed_descriptors,
                contracts,
                runtime.preference_rank,
                runtime.trusted,
                materialise_optional_value(active, registry, runtime.limits)?,
            )
            .map_err(|source| map_construction_error(source, item_path))?,
        );
    }
    let client_offer = InvocationClientOffer::new(
        request.client_offer.protocol_major,
        request.client_offer.locale,
        request.client_offer.timezone,
        sinks,
        runtimes,
        request.client_offer.maximum_frame_size,
        request.client_offer.maximum_artifact_size,
        materialise_optional_value(active, registry, request.client_offer.limits)?,
        materialise_optional_value(active, registry, request.client_offer.preferences)?,
    )
    .map_err(|source| map_construction_error(source, offer_path))?;
    let output_requirement = request
        .output
        .map(|output| materialise_output(output))
        .transpose()?;
    let input = InvokeRequestInput {
        target,
        arguments,
        caller_context: caller,
        client_offer,
        output_requirement,
        state_profile: request.state_profile.map(str::to_owned),
        trace_policy: request.trace_policy,
        idempotency_key: request.idempotency_key.map(<[u8]>::to_vec),
        parent_invocation_id: request.parent_invocation,
        observer_context: materialise_optional_value(active, registry, request.observer_context)?,
    };
    InvokeRequest::new(input).map_err(|source| {
        map_construction_error(
            source,
            InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestArguments),
        )
    })
}

fn materialise_output(
    output: ValidatedOutputWire<'_>,
) -> Result<InvocationOutputRequirement, InvocationCarrierCodecError> {
    let output_path =
        InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestOutputRequirement);
    let type_selector = output
        .type_selector
        .map(|selector| match selector {
            ValidatedOutputTypeWire::TypeId(type_id) => {
                Ok(InvocationOutputTypeSelector::type_id(type_id))
            }
            ValidatedOutputTypeWire::QualifiedName(parts) => {
                let path = output_path.with(InvocationCarrierPathSegment::OutputType);
                InvocationOutputTypeSelector::qualified_name(qualified_name(parts, path.clone())?)
                    .map_err(|source| map_construction_error(source, path))
            }
        })
        .transpose()?;
    InvocationOutputRequirement::new(
        output.alias.map(str::to_owned),
        output.media_type.map(str::to_owned),
        type_selector,
        output.streaming,
    )
    .map_err(|source| map_construction_error(source, output_path))
}

fn materialise_event(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    event: ValidatedEventWire<'_>,
) -> Result<InvokeEvent, InvocationCarrierCodecError> {
    let body_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody);
    let body = match event.body {
        ValidatedEventBodyWire::Started { visible_principal } => {
            InvocationEventBody::Started { visible_principal }
        }
        ValidatedEventBodyWire::ValueBatch { schema, values } => {
            let schema = materialise_optional_value(active, registry, schema)?;
            let mut decoded = Vec::new();
            for value in values {
                decoded.push(materialise_invoke_value(active, registry, value)?);
            }
            InvocationEventBody::value_batch(schema, decoded).map_err(|source| {
                map_construction_error(
                    source,
                    body_path.with(InvocationCarrierPathSegment::BatchValues),
                )
            })?
        }
        ValidatedEventBodyWire::Diagnostic {
            severity,
            code,
            message,
        } => InvocationEventBody::Diagnostic(
            InvocationDiagnostic::new(severity, code, message).map_err(|source| {
                map_construction_error(source, body_path.with(InvocationCarrierPathSegment::Code))
            })?,
        ),
        ValidatedEventBodyWire::Completed {
            duration_nanoseconds,
        } => InvocationEventBody::Completed {
            duration_nanoseconds,
        },
        ValidatedEventBodyWire::Failed {
            phase,
            code,
            message,
            details,
            retryability,
        } => InvocationEventBody::Failed(
            InvocationFailure::new(
                phase,
                code,
                message,
                materialise_optional_value(active, registry, details)?,
                retryability,
            )
            .map_err(|source| {
                map_construction_error(source, body_path.with(InvocationCarrierPathSegment::Code))
            })?,
        ),
        ValidatedEventBodyWire::Cancelled { reason } => {
            InvocationEventBody::cancelled(reason.map(str::to_owned)).map_err(|source| {
                map_construction_error(source, body_path.with(InvocationCarrierPathSegment::Reason))
            })?
        }
    };
    InvokeEvent::new(event.invocation_id, event.sequence, body)
        .map_err(|source| map_construction_error(source, body_path))
}
